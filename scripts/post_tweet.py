#!/usr/bin/env python3
"""
FluxDown 发版推文脚本 —— 新版本发布时向 X (Twitter) 官方账号自动发一条英文公告。

用 X API v2 `POST /2/tweets`（Free 档唯一可用的发推端点；v1.1 statuses/update
在免费档已停用），OAuth 1.0a User Context 鉴权。凭据一律来自环境变量，禁止硬编码：

  X_API_KEY             Consumer API Key
  X_API_SECRET          Consumer API Secret
  X_ACCESS_TOKEN        Access Token
  X_ACCESS_TOKEN_SECRET Access Token Secret

用法:
  python scripts/post_tweet.py --version 0.5.0 \
      --notes RELEASE_NOTES.md \
      --repo owner/fluxdown

  # 只打印将要发送的文案，不真正发推（本地调试 / dry-run）
  python scripts/post_tweet.py --version 0.5.0 --notes RELEASE_NOTES.md --dry-run

依赖: requests, requests-oauthlib（CI 里 `pip install requests requests-oauthlib` 临时安装）。
"""

import argparse
import os
import re
import sys

TWEET_LIMIT = 280
# X 会用 t.co 把任意 URL 折算成固定 23 字符，无论真实长度
TCO_LENGTH = 23
ENDPOINT = "https://api.x.com/2/tweets"


def extract_english_highlights(notes_path: str, max_items: int = 3) -> list[str]:
    """从双语 RELEASE_NOTES.md 中提取英文区块的前若干条更新条目。

    双语文件由 CI 的翻译步骤生成，形如:
        <!-- fluxdown:lang:zh -->
        ...中文...
        <!-- fluxdown:lang:en -->
        ## [0.5.0] - 2026-07-04
        ### 🚀 Features
        - *(engine)* Add ED2K protocol support
        ...
    翻译失败时会回退为无标记的原始 git-cliff 输出，此时直接扫全文。
    """
    try:
        text = open(notes_path, encoding="utf-8").read()
    except OSError:
        return []

    marker = "<!-- fluxdown:lang:en -->"
    idx = text.find(marker)
    section = text[idx + len(marker) :] if idx != -1 else text

    items: list[str] = []
    for line in section.splitlines():
        line = line.strip()
        m = re.match(r"^-\s+(.*)$", line)
        if not m:
            continue
        item = m.group(1)
        # 去掉 *(scope)* 前缀、反引号、行尾 issue/PR 链接噪音
        item = re.sub(r"\*\(([^)]*)\)\*\s*", "", item)
        item = item.replace("`", "").strip()
        item = re.sub(r"\s*\(\[[0-9a-f]{6,}\]\([^)]*\)\)\s*$", "", item)
        if item:
            items.append(item)
        if len(items) >= max_items:
            break
    return items


def build_status(version: str, repo: str, highlights: list[str]) -> str:
    """组装 ≤280 字符的推文文案。URL 按 t.co 的 23 字符折算预算。"""
    url = f"https://github.com/{repo}/releases/latest"
    header = f"🚀 FluxDown v{version} is out!"
    footer = "\n\nDownload:"
    tags = "\n\n#FluxDown #DownloadManager #Rust"

    # 固定开销：header + footer + 折算后的 URL + tags + 换行
    fixed = len(header) + len(footer) + 1 + TCO_LENGTH + len(tags)
    budget = TWEET_LIMIT - fixed

    lines: list[str] = []
    for h in highlights:
        bullet = f"\n• {h}"
        if budget - len(bullet) < 0:
            break
        lines.append(bullet)
        budget -= len(bullet)

    body = "".join(lines)
    return f"{header}{body}{footer} {url}{tags}"


def post_tweet(status: str) -> dict:
    from requests_oauthlib import OAuth1Session

    key = os.environ.get("X_API_KEY", "")
    secret = os.environ.get("X_API_SECRET", "")
    token = os.environ.get("X_ACCESS_TOKEN", "")
    token_secret = os.environ.get("X_ACCESS_TOKEN_SECRET", "")
    missing = [
        name
        for name, val in (
            ("X_API_KEY", key),
            ("X_API_SECRET", secret),
            ("X_ACCESS_TOKEN", token),
            ("X_ACCESS_TOKEN_SECRET", token_secret),
        )
        if not val
    ]
    if missing:
        raise SystemExit(f"缺少环境变量: {', '.join(missing)}")

    oauth = OAuth1Session(
        client_key=key,
        client_secret=secret,
        resource_owner_key=token,
        resource_owner_secret=token_secret,
    )
    resp = oauth.post(ENDPOINT, json={"text": status})
    if resp.status_code not in (200, 201):
        raise SystemExit(
            f"发推失败: HTTP {resp.status_code}\n{resp.text}"
        )
    return resp.json()


def main() -> None:
    parser = argparse.ArgumentParser(description="FluxDown 发版推文脚本")
    parser.add_argument("--version", required=True, help="版本号（不含 v 前缀）")
    parser.add_argument(
        "--notes", default="RELEASE_NOTES.md", help="双语 release notes 文件路径"
    )
    parser.add_argument(
        "--repo",
        default=os.environ.get("GITHUB_REPOSITORY", ""),
        help="owner/repo，默认取 GITHUB_REPOSITORY",
    )
    parser.add_argument(
        "--max-items", type=int, default=3, help="推文里最多列几条更新亮点"
    )
    parser.add_argument(
        "--dry-run", action="store_true", help="只打印文案，不真正发推"
    )
    args = parser.parse_args()

    if not args.repo:
        raise SystemExit("必须提供 --repo 或设置 GITHUB_REPOSITORY 环境变量")

    highlights = extract_english_highlights(args.notes, args.max_items)
    status = build_status(args.version, args.repo, highlights)

    print("── 推文文案 ──")
    print(status)
    print(f"── 字符数: {len(status)} (t.co 折算后 URL 计 {TCO_LENGTH}) ──")

    if args.dry_run:
        print("dry-run: 跳过实际发推")
        return

    result = post_tweet(status)
    tweet_id = result.get("data", {}).get("id", "<unknown>")
    print(f"✅ 已发推: https://x.com/i/web/status/{tweet_id}")


if __name__ == "__main__":
    main()
