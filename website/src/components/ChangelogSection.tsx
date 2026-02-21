import { useState, useEffect, useCallback, useRef } from "react";
import { motion } from "framer-motion";
import {
  History,
  Tag,
  Calendar,
  Loader2,
  ChevronDown,
  FileCode,
  FileText,
  Check,
} from "lucide-react";
import { useLocale } from "@/lib/i18n";

interface Release {
  tag: string;
  version: string;
  published_at: string;
  body: string;
}

const PER_PAGE = 10;

/** 简易 Markdown → HTML（仅处理 release notes 常用语法） */
function renderMarkdown(md: string): string {
  return md
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>")
    .replace(
      /`([^`]+)`/g,
      '<code class="px-1.5 py-0.5 rounded bg-dark-surface3 text-brand-sky text-xs font-mono">$1</code>',
    )
    .replace(
      /^### (.+)$/gm,
      '<h4 class="text-sm font-semibold text-dark-text mt-5 mb-2">$1</h4>',
    )
    .replace(
      /^## (.+)$/gm,
      '<h3 class="text-base font-semibold text-dark-text mt-6 mb-2">$1</h3>',
    )
    .replace(
      /^- (.+)$/gm,
      '<li class="ml-4 pl-1.5 text-sm text-dark-text-secondary leading-relaxed list-disc">$1</li>',
    )
    .replace(
      /((?:<li[^>]*>.*<\/li>\n?)+)/g,
      '<ul class="space-y-1 my-2">$1</ul>',
    )
    .replace(
      /^(?!<[hul])((?!<\/)[^\n]+)$/gm,
      '<p class="text-sm text-dark-text-secondary leading-relaxed">$1</p>',
    )
    .replace(/\n{3,}/g, "\n\n");
}

function formatDate(dateStr: string, locale: string): string {
  const date = new Date(dateStr);
  return date.toLocaleDateString(locale === "zh" ? "zh-CN" : "en-US", {
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}

/** 去除内联 Markdown 语法，返回纯文本；反引号内容用方括号包裹 */
function cleanInline(text: string): string {
  return text
    .replace(/\*\*(.+?)\*\*/g, "$1")
    .replace(/\*(.+?)\*/g, "$1")
    .replace(/`([^`]+)`/g, "[$1]");
}

/** 将 release body 转为适合 QQ群公告粘贴的纯文本 */
function toPlainText(release: Release, locale: string): string {
  const date = formatDate(release.published_at, locale);
  const result: string[] = [
    `【FluxDown ${release.tag} 更新日志】`,
    `📅 ${date}`,
    "",
  ];

  let counter = 0;
  let lastWasBlank = true;

  for (const raw of release.body.split("\n")) {
    const line = raw.trimEnd();
    const trimmed = line.trim();

    if (!trimmed) {
      if (!lastWasBlank) {
        result.push("");
        lastWasBlank = true;
      }
      continue;
    }
    lastWasBlank = false;

    if (trimmed.startsWith("## ")) {
      counter = 0;
      result.push(`▌ ${trimmed.slice(3).trim()}`);
    } else if (trimmed.startsWith("### ")) {
      counter = 0;
      result.push(`  ◆ ${trimmed.slice(4).trim()}`);
    } else if (trimmed.startsWith("- ")) {
      counter += 1;
      result.push(`${counter}. ${cleanInline(trimmed.slice(2).trim())}`);
    } else {
      result.push(cleanInline(trimmed));
    }
  }

  // 去掉末尾多余空行
  while (result.length > 0 && result[result.length - 1] === "") {
    result.pop();
  }

  return result.join("\n");
}

function timeAgo(dateStr: string, locale: string): string {
  const now = Date.now();
  const then = new Date(dateStr).getTime();
  const days = Math.floor((now - then) / (1000 * 60 * 60 * 24));
  if (locale === "zh") {
    if (days === 0) return "今天";
    if (days === 1) return "昨天";
    if (days < 30) return `${days} 天前`;
    if (days < 365) return `${Math.floor(days / 30)} 个月前`;
    return `${Math.floor(days / 365)} 年前`;
  }
  if (days === 0) return "today";
  if (days === 1) return "yesterday";
  if (days < 30) return `${days} days ago`;
  if (days < 365) return `${Math.floor(days / 30)} months ago`;
  return `${Math.floor(days / 365)} years ago`;
}

function CopyButtons({
  release,
  locale,
  t,
}: {
  release: Release;
  locale: string;
  t: (key: string) => string;
}) {
  const [mdState, setMdState] = useState<"idle" | "copied">("idle");
  const [textState, setTextState] = useState<"idle" | "copied">("idle");

  const copy = async (content: string, which: "md" | "text") => {
    try {
      await navigator.clipboard.writeText(content);
    } catch {
      // fallback for older browsers
      const el = document.createElement("textarea");
      el.value = content;
      el.style.position = "fixed";
      el.style.opacity = "0";
      document.body.appendChild(el);
      el.select();
      document.execCommand("copy");
      document.body.removeChild(el);
    }
    if (which === "md") {
      setMdState("copied");
      setTimeout(() => setMdState("idle"), 2000);
    } else {
      setTextState("copied");
      setTimeout(() => setTextState("idle"), 2000);
    }
  };

  return (
    <div className="ml-auto flex items-center gap-0.5 shrink-0">
      {/* Copy Markdown */}
      <button
        onClick={() => copy(release.body, "md")}
        title={t("changelog.copyMd") + " (Markdown)"}
        className="inline-flex items-center gap-1 px-2 py-1 rounded text-xs text-dark-text-muted hover:text-dark-text hover:bg-dark-surface2 transition-all duration-150 cursor-pointer select-none"
      >
        {mdState === "copied" ? (
          <Check className="w-3 h-3 text-brand-sky shrink-0" />
        ) : (
          <FileCode className="w-3 h-3 shrink-0" />
        )}
        <span className={mdState === "copied" ? "text-brand-sky" : ""}>
          {mdState === "copied" ? t("changelog.copied") : t("changelog.copyMd")}
        </span>
      </button>

      {/* Copy plain text */}
      <button
        onClick={() => copy(toPlainText(release, locale), "text")}
        title={t("changelog.copyPlain") + " (QQ群公告)"}
        className="inline-flex items-center gap-1 px-2 py-1 rounded text-xs text-dark-text-muted hover:text-dark-text hover:bg-dark-surface2 transition-all duration-150 cursor-pointer select-none"
      >
        {textState === "copied" ? (
          <Check className="w-3 h-3 text-brand-sky shrink-0" />
        ) : (
          <FileText className="w-3 h-3 shrink-0" />
        )}
        <span className={textState === "copied" ? "text-brand-sky" : ""}>
          {textState === "copied"
            ? t("changelog.copied")
            : t("changelog.copyPlain")}
        </span>
      </button>
    </div>
  );
}

export default function ChangelogSection() {
  const { locale, t } = useLocale();
  const [releases, setReleases] = useState<Release[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState("");
  const [page, setPage] = useState(1);
  const [hasMore, setHasMore] = useState(false);
  const initialFetched = useRef(false);

  // 请求某一页数据
  const fetchPage = useCallback(async (p: number, append: boolean) => {
    if (append) {
      setLoadingMore(true);
    } else {
      setLoading(true);
    }
    setError("");

    try {
      const res = await fetch(`/api/changelog?page=${p}&per_page=${PER_PAGE}`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();

      const incoming: Release[] = data.releases || [];
      setReleases((prev) => (append ? [...prev, ...incoming] : incoming));
      setHasMore(data.has_more ?? false);
      setPage(p);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
      setLoadingMore(false);
    }
  }, []);

  // 首次加载
  useEffect(() => {
    if (initialFetched.current) return;
    initialFetched.current = true;
    fetchPage(1, false);
  }, [fetchPage]);

  const handleLoadMore = () => {
    if (loadingMore || !hasMore) return;
    fetchPage(page + 1, true);
  };

  return (
    <section className="relative py-20 sm:py-28 bg-dark-bg">
      <div className="mx-auto max-w-3xl px-4 sm:px-6 lg:px-8">
        {/* Header */}
        <motion.div
          className="text-center mb-14"
          initial={{ opacity: 0, y: 20 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true }}
          transition={{ duration: 0.5 }}
        >
          <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-semibold bg-brand-sky/10 text-brand-sky border border-brand-sky/20 uppercase tracking-widest">
            <History className="w-3 h-3" />
            {t("changelog.badge")}
          </span>
          <h1 className="mt-6 text-3xl sm:text-4xl lg:text-5xl font-bold tracking-tight text-dark-text">
            {t("changelog.title")}
            <span className="bg-gradient-to-r from-brand-sky to-brand-cyan bg-clip-text text-transparent">
              {t("changelog.titleHighlight")}
            </span>
          </h1>
          <p className="mt-4 text-dark-text-secondary text-base sm:text-lg max-w-xl mx-auto">
            {t("changelog.subtitle")}
          </p>
        </motion.div>

        {/* Initial loading */}
        {loading && (
          <div className="flex items-center justify-center py-20">
            <Loader2 className="w-6 h-6 text-brand-sky animate-spin" />
          </div>
        )}

        {/* Error */}
        {error && !loading && (
          <div className="text-center py-12">
            <p className="text-sm text-danger">{t("changelog.error")}</p>
          </div>
        )}

        {/* Empty */}
        {!loading && !error && releases.length === 0 && (
          <div className="text-center py-12">
            <p className="text-sm text-dark-text-muted">
              {t("changelog.empty")}
            </p>
          </div>
        )}

        {/* Release timeline */}
        {!loading && releases.length > 0 && (
          <div className="relative">
            {/* Timeline line */}
            <div className="absolute left-[19px] top-2 bottom-2 w-px bg-dark-border hidden sm:block" />

            <div className="space-y-8">
              {releases.map((release, index) => (
                <motion.article
                  key={release.tag}
                  initial={{ opacity: 0, y: 20 }}
                  whileInView={{ opacity: 1, y: 0 }}
                  viewport={{ once: true, margin: "-50px" }}
                  transition={{
                    duration: 0.4,
                    delay: Math.min(index * 0.05, 0.3),
                  }}
                  className="relative sm:pl-12"
                >
                  {/* Timeline dot */}
                  <div className="absolute left-2.5 top-1.5 w-3 h-3 rounded-full border-2 border-brand-sky bg-dark-bg hidden sm:block" />

                  {/* Card */}
                  <div className="rounded-xl border border-dark-border bg-dark-surface1 overflow-hidden">
                    {/* Card header */}
                    <div className="flex flex-wrap items-center gap-3 px-5 py-4 border-b border-dark-border bg-dark-surface1">
                      <span className="inline-flex items-center gap-1.5 px-2.5 py-0.5 rounded-full text-xs font-semibold bg-brand-sky/10 text-brand-sky border border-brand-sky/20">
                        <Tag className="w-3 h-3" />
                        {release.tag}
                      </span>
                      <span className="inline-flex items-center gap-1.5 text-xs text-dark-text-muted">
                        <Calendar className="w-3 h-3" />
                        {formatDate(release.published_at, locale)}
                      </span>
                      <span className="text-xs text-dark-text-muted">
                        {timeAgo(release.published_at, locale)}
                      </span>
                      <CopyButtons release={release} locale={locale} t={t} />
                    </div>

                    {/* Card body */}
                    <div
                      className="px-5 py-4 changelog-body"
                      dangerouslySetInnerHTML={{
                        __html: renderMarkdown(release.body),
                      }}
                    />
                  </div>
                </motion.article>
              ))}
            </div>

            {/* Load more */}
            {hasMore && (
              <div className="flex justify-center mt-10">
                <button
                  onClick={handleLoadMore}
                  disabled={loadingMore}
                  className="inline-flex items-center gap-2 px-5 py-2.5 rounded-lg border border-dark-border bg-dark-surface1 text-sm text-dark-text-secondary hover:text-dark-text hover:bg-dark-surface2 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  {loadingMore ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <ChevronDown className="w-4 h-4" />
                  )}
                  {loadingMore
                    ? t("changelog.loading")
                    : t("changelog.loadMore")}
                </button>
              </div>
            )}
          </div>
        )}
      </div>
    </section>
  );
}
