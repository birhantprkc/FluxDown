// commit 规则单一事实来源：与 cliff.toml 的 commit_parsers 严格对齐。
// opencommit（OCO_PROMPT_MODULE=@commitlint）读取本文件，把规则注入生成 prompt，
// 保证产出的 commit 头部为 ASCII 的 `type(scope): 描述`，不被 git-cliff 的
// filter_unconventional 丢弃。描述语言不受限（跟随 OCO_LANGUAGE）。
module.exports = {
  rules: {
    // type 集合 = cliff.toml commit_parsers 识别的前缀
    'type-enum': [
      2,
      'always',
      ['feat', 'fix', 'docs', 'perf', 'refactor', 'style', 'test', 'chore', 'ci', 'revert'],
    ],
    'type-case': [2, 'always', 'lower-case'],
    'type-empty': [2, 'never'],
    'scope-case': [2, 'always', 'lower-case'],
    'subject-empty': [2, 'never'],
    'subject-full-stop': [2, 'never', '.'],
    'header-max-length': [2, 'always', 100],
    'body-leading-blank': [2, 'always'],
  },
};
