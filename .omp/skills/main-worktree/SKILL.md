---
name: main-worktree
description: >-
  在不切换当前 develop 分支的情况下，通过 git worktree 编辑 main 分支内容（hotfix），
  然后合并回 develop 保持 main⊆develop 不变式。用户说「hotfix」「改 main」「main worktree」
  「在 main 上修」「同步回 develop」时使用。关键词：worktree, main, hotfix, 热修复,
  稳定分支, 合并回 develop, 不切分支, git worktree
---

# main 分支 worktree 热修流程

主目录始终停在 `develop`；`main` 挂在仓库内 worktree `.worktrees/main`（已在 `.gitignore`）。
适用场景：需要直接修改 `main`（稳定分支）内容的 hotfix，改完必须同回合合并回 `develop`。

## 流程

```bash
# 0. worktree 不存在则创建（仅首次；已存在直接跳过）
git worktree list
git worktree add .worktrees/main main

# 1. 更新 main worktree
git -C .worktrees/main pull --ff-only   # 有远端更新时

# 2. 在 .worktrees/main 内编辑、验证、提交（Conventional Commits 中文；
#    commit/push 仍需用户明确要求 —— 红线）

# 3. 回主目录合并回 develop，恢复不变式
git merge main

# 4. 校验不变式：输出必须为空，否则违规，先修复
git log main --not develop --oneline
```

## 红线（不可违反）

- `main` 上只做 hotfix / cherry-pick，禁止直接开发新功能（功能一律在 `develop`）。
- hotfix 进 `main` 后**同一回合**必须合并回 `develop`，不许留到以后。
- 未经用户明确要求禁止 commit / push / tag；推送 v* tag 触发不可逆发布流水线。
- 稳定 tag `vX.Y.Z` 只在 main worktree（`.worktrees/main`）里打。

## 注意

- 同一分支不能同时被两个 worktree checkout；主目录保持 `develop`。
- worktree 有独立的 `target/`、`build/`、`.dart_tool/`，首次构建全量编译属正常。
- 收尾可选：`git worktree remove .worktrees/main`（长期保留也没问题）。
