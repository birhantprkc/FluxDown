#!/usr/bin/env python3
"""
Release Tag Script
==================
提取两个 tag 之间的 commit log，调用 Claude CLI 生成 Release Notes，
然后创建 annotated tag 并推送。

用法:
    python scripts/release_tag.py v0.1.7
    python scripts/release_tag.py v0.1.7 --dry-run          # 仅预览，不创建 tag
    python scripts/release_tag.py v0.1.7 --push             # 创建 tag 并推送到远程
    python scripts/release_tag.py v0.1.7 --push --github-release  # 同时创建 GitHub Release
    python scripts/release_tag.py v0.1.7 --model opus       # 使用 Opus 模型（更高质量）
    python scripts/release_tag.py v0.1.7 --lang en          # 生成英文 Release Notes
    python scripts/release_tag.py v0.1.7 --lang both        # 中英双语
    python scripts/release_tag.py v0.1.7 --update-changelog # 更新 CHANGELOG.md

特性:
    - 代码感知：AI 主动调用 git diff/show 查看真实代码变更（只读，不编辑）
    - 流式输出：AI 生成时实时打印，无需等待完整响应
    - 调用费用：每次 AI 调用后显示成本
    - 发布类型：AI 自动建议 major/minor/patch（使用 haiku 模型，速度快）
    - 模型选择：--model haiku/sonnet/opus，按质量/成本权衡
    - 多语言：--lang zh/en/both，支持双语发布说明
    - GitHub Release：--github-release 通过 gh CLI 直接创建 Release
    - CHANGELOG：--update-changelog 自动追加到 CHANGELOG.md
"""

import argparse
import json
import os
import subprocess
import sys
import textwrap
import threading
from pathlib import Path

# 项目根目录（scripts/ 的上一级），确保 claude 始终在此运行并加载 CLAUDE.md
PROJECT_DIR = Path(__file__).parent.parent

# Windows 下强制 UTF-8 输出
if sys.platform == "win32":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")  # type: ignore[union-attr]
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")  # type: ignore[union-attr]


def run(cmd: list[str], *, check: bool = True) -> str:
    """运行命令并返回 stdout"""
    env = os.environ.copy()
    env["PYTHONIOENCODING"] = "utf-8"
    result = subprocess.run(
        cmd,
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        env=env,
        cwd=str(PROJECT_DIR),
    )
    if check and result.returncode != 0:
        print(f"命令失败: {' '.join(cmd)}", file=sys.stderr)
        print(result.stderr, file=sys.stderr)
        sys.exit(1)
    return result.stdout.strip()


def get_previous_tag() -> str | None:
    """获取最新的 tag"""
    output = run(["git", "tag", "-l", "--sort=-v:refname"], check=False)
    if not output:
        return None
    return output.splitlines()[0]


def get_commits(from_tag: str | None, to_ref: str = "HEAD") -> str:
    """获取两个 ref 之间的 commit log，含变更文件列表（用于模块归属判断）"""
    range_spec = f"{from_tag}..{to_ref}" if from_tag else to_ref
    return run(["git", "log", range_spec, "--pretty=format:COMMIT %h %s", "--name-only"])


# ─── Claude CLI 调用 ─────────────────────────────────────────────────────────

def _call_claude_streaming(prompt: str, model: str = "sonnet") -> str:
    """
    通过 Claude CLI 以流式 JSON 模式调用，实时打印 token，返回最终完整文本。
    使用 --output-format stream-json + --include-partial-messages 获取实时 token。
    """
    env = os.environ.copy()
    env["PYTHONIOENCODING"] = "utf-8"

    process = subprocess.Popen(
        [
            "claude", "-p", prompt,
            "--output-format", "stream-json",
            "--include-partial-messages",
            "--model", model,
            "--max-turns", "10",
            "--allowedTools", "Bash", "Read", "Glob", "Grep",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        encoding="utf-8",
        errors="replace",
        env=env,
        cwd=str(PROJECT_DIR),
    )

    # 在后台线程中排空 stderr，防止缓冲区死锁
    stderr_lines: list[str] = []

    def _drain_stderr() -> None:
        assert process.stderr is not None
        for line in process.stderr:
            stderr_lines.append(line)

    stderr_thread = threading.Thread(target=_drain_stderr, daemon=True)
    stderr_thread.start()

    final_result = ""
    collected: list[str] = []

    assert process.stdout is not None
    for raw in process.stdout:
        line = raw.strip()
        if not line:
            continue
        try:
            evt = json.loads(line)
        except json.JSONDecodeError:
            continue

        etype = evt.get("type")

        if etype == "stream_event":
            # --include-partial-messages 产生的实时 token 事件
            delta = evt.get("event", {}).get("delta", {})
            if delta.get("type") == "text_delta":
                text = delta.get("text", "")
                if text:
                    print(text, end="", flush=True)
                    collected.append(text)

        elif etype == "result":
            # 最终结果，包含完整文本和费用信息
            res = evt.get("result", "")
            cost = evt.get("total_cost_usd")
            if cost is not None:
                print(f"\n[费用: ${cost:.4f}]", flush=True)
            if res:
                final_result = res

    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()

    stderr_thread.join(timeout=5)

    if process.returncode != 0:
        stderr = "".join(stderr_lines)
        print(f"\nClaude CLI 调用失败: {stderr}", file=sys.stderr)
        sys.exit(1)

    return final_result or "".join(collected).strip()


def _call_claude_simple(prompt: str, model: str = "haiku") -> str:
    """非流式调用 Claude CLI（用于快速分类等轻量任务）"""
    env = os.environ.copy()
    env["PYTHONIOENCODING"] = "utf-8"
    result = subprocess.run(
        ["claude", "-p", prompt, "--model", model, "--max-turns", "1"],
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        env=env,
        timeout=60,
        cwd=str(PROJECT_DIR),
    )
    if result.returncode != 0:
        return ""
    return result.stdout.strip()


def suggest_release_type(commits: str, prev_tag: str | None) -> str:
    """让 AI 建议语义化版本类型（major / minor / patch），使用 haiku 快速响应"""
    prompt = textwrap.dedent(f"""\
        根据以下 Git commit 记录，判断本次发布属于哪种语义化版本类型：

        - **major**：有破坏性变更，用户需要修改配置或工作流
        - **minor**：新增功能，向后兼容
        - **patch**：仅修复 bug 或微小改进，无新功能

        上一个版本：{prev_tag or '(无)'}
        Commit 记录：
        {commits}

        只输出一个单词：major、minor 或 patch。不要有任何其他内容。
    """)
    output = _call_claude_simple(prompt, model="haiku").lower()
    for word in ("major", "minor", "patch"):
        if word in output:
            return word
    return "unknown"


# ─── Release Notes 生成 ──────────────────────────────────────────────────────

def _build_notes_prompt(version: str, commits: str, prev_tag: str | None, lang: str) -> str:
    """构建生成 Release Notes 的完整 prompt"""
    range_desc = f"{prev_tag}..{version}" if prev_tag else f"初始版本到 {version}"
    range_spec = f"{prev_tag}..HEAD" if prev_tag else "HEAD"

    if lang == "en":
        lang_note = "Write entirely in English."
        section_client = "### Client"
        section_ext = "### Browser Extension"
        section_web = "### Website"
    elif lang == "both":
        lang_note = (
            "Write in Chinese first (主要内容). "
            "Then add an `## English` section at the bottom with an English translation."
        )
        section_client = "### 客户端 / ### Client"
        section_ext = "### 浏览器扩展 / ### Browser Extension"
        section_web = "### 官网 / ### Website"
    else:
        lang_note = "用中文撰写。"
        section_client = "### 客户端"
        section_ext = "### 浏览器扩展"
        section_web = "### 官网"

    return textwrap.dedent(f"""\
        你是 FluxDown 项目的发布助手。FluxDown 是一个多协议下载工具，项目包含三个模块：

        1. **客户端** — 桌面下载应用（Flutter + Rust），支持 HTTP/FTP/BT 多协议下载
        2. **浏览器扩展** — Chrome/Firefox 扩展，用于拦截浏览器下载并发送到客户端
        3. **官网** — FluxDown 官方网站，产品展示与下载页

        ## 文件路径 → 模块归属规则（优先级高于 commit 消息措辞）

        | 文件路径前缀 | 所属模块 |
        |-------------|---------|
        | `lib/` `native/` `windows/` `android/` `ios/` `macos/` `linux/` `assets/` | 客户端 |
        | `fluxDown/` | 浏览器扩展 |
        | `website/` | 官网 |
        | `.github/workflows/` `scripts/` 根目录配置文件 | 忽略（开发工具，用户不感知） |

        跨模块 commit 需拆分到各自模块下分别描述。

        ## 可用工具（请主动调用以理解实际代码变更）

        你有权调用 Bash / Read / Glob / Grep 工具查看代码，**但不得编辑任何文件**。
        仅凭 commit 消息往往不足以准确描述功能改动，请通过以下命令查看真实变更：

        ```bash
        # 查看本次发布的整体变更统计（先从这里入手）
        git diff --stat {range_spec}

        # 按模块查看完整 diff
        git diff {range_spec} -- lib/
        git diff {range_spec} -- native/hub/src/
        git diff {range_spec} -- fluxDown/

        # 查看单个 commit 的详细变更
        git show <hash>

        # 搜索关键实现（如某功能的具体逻辑）
        git log {range_spec} --follow -p -- <具体文件路径>
        ```

        **分析步骤**：
        1. 先运行 `git diff --stat {range_spec}` 了解变更范围
        2. 对各模块分别运行 `git diff` 查看代码细节
        3. 对重要 commit 运行 `git show <hash>` 深入理解改动意图
        4. 基于**实际代码内容**撰写 Release Notes，commit 消息仅作辅助参考

        ## 任务

        {lang_note}
        通过查看实际代码变更，生成一份面向用户的 Release Notes。

        以下是 commit 概要（含变更文件列表），供初步定位用：

        版本: {version}
        变更范围: {range_desc}

        Commit 概要:
        ```
        {commits}
        ```

        ## 输出要求

        1. Markdown 格式
        2. 开头一两句话概括核心亮点
        3. 若有破坏性变更，在顶部以 `> ⚠️ **Breaking Changes**` 引用块列出
        4. 按模块分组（仅列出有实际文件变更的模块）：
           - {section_client}
           - {section_ext}
           - {section_web}
        5. 每模块内按类型（新功能 / 改进 / 修复）用列表描述，语言简洁
        6. 忽略纯 CI/chore/scripts commit
        7. 只输出正文，不加多余解释
        8. 禁止在开头加版本号标题（如 "## v0.1.7 Release Notes"），直接从总结句开始
        9. 不要使用水平分隔线（---）
        10. 不要总结匿名信息收集方面的内容
        11. 不要携带代码片段，只总结功能
    """)


def generate_release_notes(
    version: str, commits: str, prev_tag: str | None, lang: str = "zh", model: str = "sonnet"
) -> str:
    """调用 Claude CLI 流式生成 Release Notes"""
    prompt = _build_notes_prompt(version, commits, prev_tag, lang)

    print(f"正在生成 Release Notes（模型: {model}，语言: {lang}）...\n")
    print("─" * 60)

    result = _call_claude_streaming(prompt, model=model)

    print("\n" + "─" * 60 + "\n")

    if not result or result.startswith("Error:"):
        print(f"Claude CLI 返回错误: {result}", file=sys.stderr)
        sys.exit(1)

    return result


def refine_release_notes(current_notes: str, user_request: str, model: str = "sonnet") -> str:
    """根据用户反馈调用 Claude CLI 修改 Release Notes"""
    prompt = textwrap.dedent(f"""\
        你是 FluxDown 项目的发布助手。以下是当前的 Release Notes 草稿：

        ---
        {current_notes}
        ---

        用户希望做以下修改：
        {user_request}

        请按要求修改后，输出完整的 Release Notes。
        只输出正文，不加多余解释，不加版本号标题，不使用水平分隔线（---）。
    """)

    print("正在修改 Release Notes...\n")
    print("─" * 60)
    result = _call_claude_streaming(prompt, model=model)
    print("\n" + "─" * 60 + "\n")

    if not result or result.startswith("Error:"):
        print(f"Claude CLI 返回错误: {result}", file=sys.stderr)
        sys.exit(1)

    return result


# ─── 交互式审阅 ──────────────────────────────────────────────────────────────

def interactive_review(release_notes: str, model: str = "sonnet") -> str | None:
    """
    交互式审阅 Release Notes，支持多轮修改。
    返回最终确认的内容，或 None 表示取消。
    """
    current_notes = release_notes
    round_num = 1

    while True:
        print(f"\n{'═' * 60}")
        print(f"  Release Notes（第 {round_num} 版）")
        print(f"{'═' * 60}\n")
        print(current_notes)
        print(f"\n{'═' * 60}")
        print("\n操作选项:")
        print("  [y] 确认，使用此内容创建 tag")
        print("  [m] 修改，告诉 AI 需要哪些调整")
        print("  [n] 取消，退出脚本")
        print()

        choice = input("请选择 [y/m/n]: ").strip().lower()

        if choice == "y":
            return current_notes
        elif choice == "n":
            print("已取消")
            return None
        elif choice == "m":
            print("\n请描述修改要求（按回车两次提交）:")
            lines = []
            while True:
                line = input()
                if line == "" and lines and lines[-1] == "":
                    break
                lines.append(line)
            user_request = "\n".join(lines).strip()
            if not user_request:
                print("未输入修改要求，请重试")
                continue
            current_notes = refine_release_notes(current_notes, user_request, model=model)
            round_num += 1
        else:
            print("无效输入，请输入 y、m 或 n")


# ─── Git / GitHub 操作 ───────────────────────────────────────────────────────

def create_tag(version: str, message: str) -> None:
    """创建 annotated tag（使用 --cleanup=verbatim 保留 # 开头的 Markdown 标题）"""
    run(["git", "tag", "-a", version, "-m", message, "--cleanup=verbatim"])
    print(f"✓ 已创建 annotated tag: {version}")


def push_tag(version: str) -> None:
    """推送 tag 到远程"""
    run(["git", "push", "origin", version])
    print(f"✓ 已推送 tag: {version}")


def create_github_release(version: str, notes: str, prerelease: bool = False) -> None:
    """通过 gh CLI 创建 GitHub Release（tag 必须已推送）"""
    cmd = ["gh", "release", "create", version, "--notes", notes]
    if prerelease:
        cmd.append("--prerelease")

    result = subprocess.run(
        cmd,
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        cwd=str(PROJECT_DIR),
    )
    if result.returncode != 0:
        print(f"⚠ GitHub Release 创建失败:\n{result.stderr}", file=sys.stderr)
        print(f"  请手动运行: gh release create {version} --notes '...'", file=sys.stderr)
    else:
        url = result.stdout.strip()
        print(f"✓ 已创建 GitHub Release: {url}")


def update_changelog(version: str, notes: str) -> None:
    """将 Release Notes 追加到 CHANGELOG.md 顶部"""
    import datetime

    changelog_path = PROJECT_DIR / "CHANGELOG.md"
    today = datetime.date.today().isoformat()
    entry = f"## {version} — {today}\n\n{notes}\n\n"

    if changelog_path.exists():
        existing = changelog_path.read_text(encoding="utf-8")
        # 若有 # 标题行，在第一个 ## 前插入
        lines = existing.splitlines(keepends=True)
        insert_at = 0
        if lines and lines[0].startswith("# "):
            insert_at = 1
            while insert_at < len(lines) and not lines[insert_at].startswith("##"):
                insert_at += 1
        new_content = "".join(lines[:insert_at]) + "\n" + entry + "".join(lines[insert_at:])
    else:
        new_content = f"# Changelog\n\n{entry}"

    changelog_path.write_text(new_content, encoding="utf-8")
    print(f"✓ 已更新 CHANGELOG.md")


# ─── 主流程 ──────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(
        description="生成 AI Release Notes 并创建 tag",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=textwrap.dedent("""\
            示例:
              python scripts/release_tag.py v0.1.7
              python scripts/release_tag.py v0.1.7 --dry-run
              python scripts/release_tag.py v0.1.7 --push --github-release
              python scripts/release_tag.py v0.1.7 --model opus --lang both
        """),
    )
    parser.add_argument("version", help="版本号 (如 v0.1.7)")
    parser.add_argument("--dry-run", action="store_true", help="仅预览，不创建 tag")
    parser.add_argument("--push", action="store_true", help="创建后自动推送 tag")
    parser.add_argument(
        "--github-release",
        action="store_true",
        help="推送后通过 gh CLI 创建 GitHub Release（需安装 gh 并已登录）",
    )
    parser.add_argument("--prerelease", action="store_true", help="标记为 Pre-release")
    parser.add_argument(
        "--model",
        choices=["haiku", "sonnet", "opus"],
        default="sonnet",
        help="AI 模型 (默认: sonnet)，opus 质量最高，haiku 速度最快",
    )
    parser.add_argument(
        "--lang",
        choices=["zh", "en", "both"],
        default="zh",
        help="Release Notes 语言 (默认: zh 中文)",
    )
    parser.add_argument(
        "--update-changelog",
        action="store_true",
        help="将 Release Notes 追加到 CHANGELOG.md 顶部",
    )
    parser.add_argument(
        "--skip-suggest",
        action="store_true",
        help="跳过 AI 发布类型建议（major/minor/patch）",
    )
    args = parser.parse_args()

    version: str = args.version
    if not version.startswith("v"):
        version = f"v{version}"

    # ── 检查 tag 是否已存在 ──
    existing_tags = run(["git", "tag", "-l", version], check=False)
    if version in existing_tags.splitlines():
        print(f"错误: tag {version} 已存在", file=sys.stderr)
        sys.exit(1)

    # ── 获取前一个 tag 和 commit log ──
    prev_tag = get_previous_tag()
    commits = get_commits(prev_tag)

    if not commits:
        print("没有找到新的 commit", file=sys.stderr)
        sys.exit(1)

    commit_count = sum(1 for line in commits.splitlines() if line.startswith("COMMIT "))

    # ── 显示发布概况 ──
    print(f"\n{'─' * 50}")
    print(f"  版本       : {version}")
    print(f"  前一个 tag : {prev_tag or '(无)'}")
    print(f"  新增 commit: {commit_count} 个")
    print(f"  AI 模型    : {args.model}")
    print(f"  输出语言   : {args.lang}")

    print(f"{'─' * 50}\n")

    # ── AI 发布类型建议（使用 haiku 快速判断）──
    if not args.skip_suggest:
        print("正在分析发布类型（haiku）...", end=" ", flush=True)
        release_type = suggest_release_type(commits, prev_tag)
        type_labels = {"major": "重大版本 ⚠", "minor": "功能版本", "patch": "修复版本", "unknown": "未知"}
        print(f"建议: {release_type} — {type_labels.get(release_type, '')}")
        print()

    # ── 生成 Release Notes（流式输出）──
    release_notes = generate_release_notes(version, commits, prev_tag, lang=args.lang, model=args.model)

    if args.dry_run:
        print("(dry-run 模式，未创建 tag)\n")
        return

    # ── 交互式审阅 ──
    final_notes = interactive_review(release_notes, model=args.model)
    if final_notes is None:
        return

    # ── 创建 tag ──
    create_tag(version, final_notes)

    # ── 可选：更新 CHANGELOG.md ──
    if args.update_changelog:
        update_changelog(version, final_notes)

    # ── 可选：推送 tag ──
    if args.push:
        push_tag(version)

        # ── 可选：创建 GitHub Release ──
        if args.github_release:
            print()
            create_github_release(version, final_notes, prerelease=args.prerelease)
    else:
        print(f"\n提示: 运行 `git push origin {version}` 推送 tag 触发 CI")
        if args.github_release:
            print(f"      推送后运行 `gh release create {version}` 创建 GitHub Release")


if __name__ == "__main__":
    main()
