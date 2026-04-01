"""
FluxDown Auto Triage Script
- 调用 Claude 判断重复 issue + 分类 Priority/Module
- 重复 issue: 自动评论、加 duplicate 标签、关闭
- 正常 issue: 加入 Project board，设置 Status/Priority/Module
"""

import os
import json
import sys
import urllib.request
import urllib.error

# ── Project 常量 ────────────────────────────────────────────────────────────
PROJECT_ID = "PVT_kwHOCZ7LWM4BTV8N"
STATUS_FIELD_ID = "PVTSSF_lAHOCZ7LWM4BTV8NzhAnJeA"
PRIORITY_FIELD_ID = "PVTSSF_lAHOCZ7LWM4BTV8NzhApA98"
MODULE_FIELD_ID = "PVTSSF_lAHOCZ7LWM4BTV8NzhApA_8"
STATUS_TODO = "f75ad846"

PRIORITY_IDS = {
    "P0 Critical": "324fb307",
    "P1 High": "74b1faf4",
    "P2 Medium": "0276c177",
    "P3 Low": "bbadaf67",
}
MODULE_IDS = {
    "Rust Engine": "b8a8a527",
    "Flutter UI": "ee1be8bd",
    "Extension": "7d614306",
    "BT": "ab01394f",
    "HLS/DASH": "f7f5fead",
    "FTP": "686e53c7",
    "Website": "ba7293bd",
    "Infra": "0e3ece63",
}

# Logo/Vote issue 前缀，不参与重复检测
SKIP_PREFIXES = ("[Logo]", "[FluxDown] Logo", "[Logo Vote]")


# ── Env vars ────────────────────────────────────────────────────────────────
title = os.environ["ISSUE_TITLE"]
body = os.environ.get("ISSUE_BODY", "")[:1200]
labels_str = os.environ.get("ISSUE_LABELS", "")
node_id = os.environ["ISSUE_NODE_ID"]
issue_number = int(os.environ["ISSUE_NUMBER"])
repo = os.environ["REPO"]
token = os.environ["PROJECT_TOKEN"]
anthropic_key = os.environ["ANTHROPIC_API_KEY"]


# ── HTTP helpers ────────────────────────────────────────────────────────────
def gh_rest(method, path, data=None):
    req = urllib.request.Request(
        "https://api.github.com" + path,
        method=method,
        headers={
            "Authorization": "Bearer " + token,
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "Content-Type": "application/json",
        },
    )
    if data:
        req.data = json.dumps(data).encode()
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def gql(query, variables=None):
    payload = {"query": query}
    if variables:
        payload["variables"] = variables
    req = urllib.request.Request(
        "https://api.github.com/graphql",
        method="POST",
        headers={
            "Authorization": "Bearer " + token,
            "Content-Type": "application/json",
        },
        data=json.dumps(payload).encode(),
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        resp = json.loads(r.read())
    if "errors" in resp:
        raise RuntimeError("GraphQL error: " + str(resp["errors"]))
    return resp["data"]


def set_field(item_id, field_id, option_id):
    gql(
        "mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){"
        "  updateProjectV2ItemFieldValue(input:{"
        "    projectId:$p,itemId:$i,fieldId:$f,"
        "    value:{singleSelectOptionId:$o}"
        "  }){projectV2Item{id}}"
        "}",
        {"p": PROJECT_ID, "i": item_id, "f": field_id, "o": option_id},
    )


# ── 1. 拉取现有 user-feedback open issues（用于重复检测）──────────────────────
try:
    existing = gh_rest(
        "GET",
        "/repos/" + repo + "/issues"
        "?state=open&labels=user-feedback&per_page=100&sort=created&direction=desc",
    )
    candidates = [
        i
        for i in existing
        if i["number"] != issue_number
        and not any(i["title"].startswith(p) for p in SKIP_PREFIXES)
    ]
except Exception as e:
    print("[warn] Failed to fetch existing issues:", e)
    candidates = []

valid_numbers = {i["number"] for i in candidates}
candidate_list = "\n".join(
    "#" + str(i["number"]) + ": " + i["title"][:80] for i in candidates[:80]
)
print("[triage] Loaded", len(candidates), "candidates for duplicate check")


# ── 2. 构建 Claude prompt ────────────────────────────────────────────────────
lines = [
    "You are triaging GitHub issues for FluxDown, a Rust+Flutter download manager.",
    "",
    "NEW ISSUE #" + str(issue_number) + ":",
    "Title: " + title,
    "Body: " + body,
    "Labels: " + labels_str,
    "",
]

if candidate_list:
    lines += [
        "EXISTING OPEN ISSUES (newest first):",
        candidate_list,
        "",
        "DUPLICATE CHECK:",
        "Flag as duplicate ONLY if describing the EXACT same problem or feature request.",
        "Similar topics are NOT duplicates (e.g. two separate BT issues with different details).",
        "",
    ]

lines += [
    "CLASSIFICATION:",
    "Priority: P0 Critical (crash/data-loss) | P1 High (major broken) | P2 Medium (normal) | P3 Low (minor)",
    "Module: Rust Engine | Flutter UI | Extension | BT | HLS/DASH | FTP | Website | Infra",
    "",
    "Infra covers: installer, startup crash, auto-update, env/dependency issues.",
    "Extension covers: Chrome/Firefox/Edge extension, NMH, download interception.",
    "",
    "Respond with VALID JSON only, no markdown:",
    '{"duplicate_of": null, "priority": "P2 Medium", "module": "Flutter UI"}',
]

prompt = "\n".join(lines)


# ── 3. 调用 Claude Haiku ─────────────────────────────────────────────────────
result = {"duplicate_of": None, "priority": "P2 Medium", "module": "Flutter UI"}

try:
    req = urllib.request.Request(
        "https://api.anthropic.com/v1/messages",
        method="POST",
        headers={
            "x-api-key": anthropic_key,
            "anthropic-version": "2023-06-01",
            "content-type": "application/json",
        },
        data=json.dumps(
            {
                "model": "claude-haiku-4-5",
                "max_tokens": 120,
                "messages": [{"role": "user", "content": prompt}],
            }
        ).encode(),
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        raw = json.loads(r.read())["content"][0]["text"].strip()

    # 防御：去掉 claude 偶尔返回的 markdown code fence
    if raw.startswith("```"):
        raw = raw.split("```")[1]
        if raw.startswith("json"):
            raw = raw[4:]

    parsed = json.loads(raw.strip())
    result.update(parsed)
    print("[triage] Claude result:", result)

except Exception as e:
    print("[warn] Claude API failed:", e, "- using defaults")

# 防御：校验字段值合法性
if result.get("priority") not in PRIORITY_IDS:
    result["priority"] = "P2 Medium"
if result.get("module") not in MODULE_IDS:
    result["module"] = "Flutter UI"

# 防御：Claude 返回的 duplicate_of 必须在已知候选列表中
if result.get("duplicate_of") and int(result["duplicate_of"]) not in valid_numbers:
    print(
        "[warn] Claude returned unknown duplicate #"
        + str(result["duplicate_of"])
        + ", ignoring"
    )
    result["duplicate_of"] = None


# ── 4a. 重复 issue 处理 ──────────────────────────────────────────────────────
if result.get("duplicate_of"):
    dup_num = int(result["duplicate_of"])
    print("[triage] Duplicate of #" + str(dup_num))

    # 加 duplicate 标签
    gh_rest(
        "POST",
        "/repos/" + repo + "/issues/" + str(issue_number) + "/labels",
        {"labels": ["duplicate"]},
    )

    # 自动评论
    comment = (
        "感谢您的反馈！\n\n"
        "经自动检测，此问题与 #" + str(dup_num) + " 描述的内容高度相似，"
        "已将本 issue 标记为重复并关闭。\n\n"
        "请前往 #"
        + str(dup_num)
        + " 关注进展，也欢迎在原 issue 中补充更多细节 :pray:\n\n"
        "> *此回复由自动分诊程序生成*"
    )
    gh_rest(
        "POST",
        "/repos/" + repo + "/issues/" + str(issue_number) + "/comments",
        {"body": comment},
    )

    # 关闭 issue
    gh_rest(
        "PATCH",
        "/repos/" + repo + "/issues/" + str(issue_number),
        {"state": "closed", "state_reason": "not_planned"},
    )

    print("[triage] Done - marked duplicate of #" + str(dup_num))
    sys.exit(0)


# ── 4b. 正常分诊：加入 Project board ─────────────────────────────────────────
add_result = gql(
    "mutation($p:ID!,$c:ID!){"
    "  addProjectV2ItemById(input:{projectId:$p,contentId:$c}){"
    "    item{id}"
    "  }"
    "}",
    {"p": PROJECT_ID, "c": node_id},
)
item_id = add_result["addProjectV2ItemById"]["item"]["id"]
print("[triage] Added to project, item=" + item_id)

set_field(item_id, STATUS_FIELD_ID, STATUS_TODO)
set_field(item_id, PRIORITY_FIELD_ID, PRIORITY_IDS[result["priority"]])
set_field(item_id, MODULE_FIELD_ID, MODULE_IDS[result["module"]])

print("[triage] Done - priority=" + result["priority"] + " module=" + result["module"])
