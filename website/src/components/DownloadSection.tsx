import { useState, useEffect, useCallback, type ComponentType } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { Download, Check, Loader2, ChevronDown, Puzzle, TrendingUp, Bell, CheckCircle2, AlertCircle, Globe, Smartphone } from "lucide-react";
import { SiApple, SiLinux } from "@icons-pack/react-simple-icons";
import { LampEffect } from "@/components/ui/lamp-effect";
import { useLocale } from "@/lib/i18n";

const techStack = [
  { name: "Flutter", color: "text-brand-sky" },
  { name: "Rust", color: "text-[#dea584]" },
  { name: "Tokio", color: "text-brand-cyan" },
  { name: "SQLite", color: "text-success" },
];

/* Windows logo — not available in Simple Icons (trademark), use inline SVG */
function WindowsLogo({ className }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="currentColor">
      <path d="M0 3.449L9.75 2.1v9.451H0m10.949-9.602L24 0v11.4H10.949M0 12.6h9.75v9.451L0 20.699M10.949 12.6H24V24l-13.051-1.849" />
    </svg>
  );
}

interface ReleaseAsset {
  name: string;
  size: number;
  download_url: string;
}

interface ReleaseInfo {
  version: string;
  tag: string;
  published_at: string;
  total_downloads: number;
  assets: {
    setup: ReleaseAsset | null;
    portable: ReleaseAsset | null;
    extension: ReleaseAsset | null;
  };
}

function formatSize(bytes: number): string {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export default function DownloadSection() {
  const { t } = useLocale();
  const [release, setRelease] = useState<ReleaseInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [showPortable, setShowPortable] = useState(false);

  useEffect(() => {
    fetch("/api/release")
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then((data: ReleaseInfo) => setRelease(data))
      .catch((err) => console.error("Failed to fetch release info:", err))
      .finally(() => setLoading(false));
  }, []);

  const [subscribeTarget, setSubscribeTarget] = useState<string | null>(null);
  const [subscribeEmail, setSubscribeEmail] = useState("");
  const [subscribeStatus, setSubscribeStatus] = useState<"idle" | "loading" | "success" | "duplicate" | "error">("idle");

  const handleSubscribe = useCallback(async (platform: string) => {
    if (!subscribeEmail.trim()) return;
    setSubscribeStatus("loading");
    try {
      const res = await fetch("/api/subscribe", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ email: subscribeEmail.trim(), platform }),
      });
      if (res.status === 429) { setSubscribeStatus("error"); return; }
      if (!res.ok) { setSubscribeStatus("error"); return; }
      const data = await res.json();
      setSubscribeStatus(data.message === "already_subscribed" ? "duplicate" : "success");
      if (data.message !== "already_subscribed") setSubscribeEmail("");
      setTimeout(() => { setSubscribeStatus("idle"); setSubscribeTarget(null); }, 4000);
    } catch {
      setSubscribeStatus("error");
    }
  }, [subscribeEmail]);

  const platforms: { key: string; name: string; icon: ComponentType<{ className?: string; size?: number; color?: string }>; arch: string; available: boolean; primary: boolean; badge: string }[] = [
    { key: "windows", name: t("dl.windows"), icon: WindowsLogo, arch: "x64", available: true, primary: true, badge: t("dl.availableNow") },
    { key: "macos", name: t("dl.macos"), icon: SiApple, arch: "Apple Silicon", available: false, primary: false, badge: t("dl.comingSoon") },
    { key: "linux", name: t("dl.linux"), icon: SiLinux, arch: "x64", available: false, primary: false, badge: t("dl.comingSoon") },
    { key: "web", name: t("dl.web"), icon: Globe, arch: t("dl.webArch"), available: false, primary: false, badge: t("dl.comingSoon") },
    { key: "mobile", name: t("dl.mobile"), icon: Smartphone, arch: "Android / iOS", available: false, primary: false, badge: t("dl.comingSoon") },
  ];

  return (
    <section id="download" className="relative pt-16 sm:pt-20 pb-20 sm:pb-32 overflow-hidden bg-dark-bg">
      <LampEffect>
        <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8 relative z-10">
          <motion.div
            className="text-center max-w-2xl mx-auto mb-16"
            initial={{ opacity: 0, y: 20 }}
            whileInView={{ opacity: 1, y: 0 }}
            viewport={{ once: true }}
            transition={{ duration: 0.5 }}
          >
            <span className="inline-flex items-center px-3 py-1 rounded-full text-xs font-semibold bg-brand-blue/10 text-brand-blue border border-brand-blue/20 uppercase tracking-widest">
              {t("dl.badge")}
            </span>
            <h2 className="mt-6 text-3xl sm:text-4xl lg:text-5xl font-bold tracking-tight text-dark-text">
              {t("dl.title")}
              <span className="bg-gradient-to-r from-brand-sky to-brand-cyan bg-clip-text text-transparent">{t("dl.titleHighlight")}</span>?
            </h2>
            <p className="mt-4 text-dark-text-secondary text-lg">
              {t("dl.subtitle")}
            </p>
          </motion.div>

          {/* Platform cards */}
          <motion.div
            className="flex flex-wrap justify-center gap-5 max-w-4xl mx-auto mb-16"
            initial={{ opacity: 0, y: 30 }}
            whileInView={{ opacity: 1, y: 0 }}
            viewport={{ once: true }}
            transition={{ duration: 0.6, delay: 0.1 }}
          >
            {platforms.map((p, i) => {
              const Icon = p.icon;
              return (
                <motion.div
                  key={p.name}
                  initial={{ opacity: 0, y: 20 }}
                  whileInView={{ opacity: 1, y: 0 }}
                  viewport={{ once: true }}
                  transition={{ delay: 0.1 * i, duration: 0.5 }}
                  className={`relative group rounded-xl border p-6 text-center w-full sm:w-[calc(33.333%-14px)] ${
                    p.primary
                      ? "border-brand-blue/30 bg-gradient-to-b from-dark-surface1 to-dark-surface2 hover:border-brand-blue/50 transition-colors duration-300"
                      : "border-dark-border/60 bg-dark-surface1 hover:-translate-y-1 hover:border-dark-text-muted/20 hover:shadow-lg hover:shadow-black/20 transition-all duration-300 ease-out"
                  }`}
                >
                  {p.primary ? (
                    <div className="absolute -top-2.5 left-1/2 -translate-x-1/2 px-3 py-0.5 rounded-full bg-brand-blue text-[10px] font-semibold text-white flex items-center gap-1 whitespace-nowrap">
                      <Check className="w-3 h-3" />
                      {p.badge}
                    </div>
                  ) : (
                    <div className="absolute -top-2.5 left-1/2 -translate-x-1/2 px-3 py-0.5 rounded-full border border-dashed border-dark-text-muted/30 bg-dark-surface1 text-[10px] font-medium text-dark-text-muted flex items-center gap-1 whitespace-nowrap">
                      {p.badge}
                    </div>
                  )}
                  <div className={`w-14 h-14 rounded-xl flex items-center justify-center mx-auto mb-4 transition-all duration-300 ${
                    p.primary
                      ? "bg-gradient-to-br from-brand-sky to-brand-cyan"
                      : "bg-dark-surface2 border border-dark-border/50 group-hover:border-brand-blue/20 group-hover:bg-gradient-to-br group-hover:from-brand-blue/10 group-hover:to-brand-cyan/5"
                  }`}>
                    <Icon
                      className={`w-7 h-7 transition-colors duration-300 ${
                        p.primary
                          ? "text-white"
                          : "text-dark-text-muted group-hover:text-brand-blue/70"
                      }`}
                      color="currentColor"
                    />
                  </div>
                  <h3 className="text-base font-semibold text-dark-text">{p.name}</h3>
                  <p className="text-xs text-dark-text-muted mt-1">{p.arch}</p>

                  {/* 版本号 */}
                  {p.primary && release && (
                    <p className="text-[10px] text-dark-text-muted mt-1">
                      {t("dl.version", { version: release.version })}
                      {release.assets.setup && (
                        <span className="ml-1.5">({formatSize(release.assets.setup.size)})</span>
                      )}
                    </p>
                  )}

                  {p.available ? (
                    <div className="mt-4 flex flex-col gap-2">
                      {/* 主下载按钮（安装包） */}
                      {loading ? (
                        <div className="inline-flex items-center justify-center gap-2 w-full rounded-lg bg-brand-blue/50 px-5 py-2.5 text-xs font-semibold text-white/70 cursor-wait">
                          <Loader2 className="w-3.5 h-3.5 animate-spin" />
                          {t("dl.loading")}
                        </div>
                      ) : release?.assets.setup ? (
                        <a
                          href={release.assets.setup.download_url}
                          className="inline-flex items-center justify-center gap-2 w-full rounded-lg bg-brand-blue px-5 py-2.5 text-xs font-semibold text-white hover:bg-brand-blue/90 transition-colors shadow-lg shadow-brand-blue/20"
                        >
                          <Download className="w-3.5 h-3.5" />
                          {t("dl.downloadBtn")} — {t("dl.installPkg")}
                        </a>
                      ) : (
                        <a
                          href="#"
                          className="inline-flex items-center justify-center gap-2 w-full rounded-lg bg-brand-blue px-5 py-2.5 text-xs font-semibold text-white hover:bg-brand-blue/90 transition-colors shadow-lg shadow-brand-blue/20"
                        >
                          <Download className="w-3.5 h-3.5" />
                          {t("dl.downloadBtn")}
                        </a>
                      )}

                      {/* 便携版下载（折叠） */}
                      {release?.assets.portable && (
                        <>
                          <button
                            type="button"
                            onClick={() => setShowPortable(!showPortable)}
                            className="inline-flex items-center justify-center gap-1 text-[10px] text-dark-text-muted hover:text-dark-text-secondary transition-colors"
                          >
                            {t("dl.portablePkg")}
                            <ChevronDown className={`w-3 h-3 transition-transform ${showPortable ? "rotate-180" : ""}`} />
                          </button>
                          {showPortable && (
                            <a
                              href={release.assets.portable.download_url}
                              className="inline-flex items-center justify-center gap-2 w-full rounded-lg border border-dark-border px-5 py-2 text-[10px] font-medium text-dark-text-secondary hover:bg-dark-surface3 transition-colors"
                            >
                              <Download className="w-3 h-3" />
                              {t("dl.portablePkg")} ({formatSize(release.assets.portable.size)})
                            </a>
                          )}
                        </>
                      )}
                    </div>
                  ) : (
                    <div className="mt-4 flex flex-col gap-2 w-full">
                      <AnimatePresence mode="wait">
                        {subscribeTarget === p.key ? (
                          <motion.div
                            key="subscribe-form"
                            initial={{ opacity: 0, height: 0 }}
                            animate={{ opacity: 1, height: "auto" }}
                            exit={{ opacity: 0, height: 0 }}
                            transition={{ duration: 0.2 }}
                            className="flex flex-col gap-2"
                          >
                            {subscribeStatus === "success" ? (
                              <div className="flex items-center justify-center gap-1.5 rounded-lg border border-success/30 bg-success/10 px-4 py-2.5 text-xs font-medium text-success">
                                <CheckCircle2 className="w-3.5 h-3.5" />
                                {t("dl.subscribed")}
                              </div>
                            ) : subscribeStatus === "duplicate" ? (
                              <div className="flex items-center justify-center gap-1.5 rounded-lg border border-brand-blue/30 bg-brand-blue/10 px-4 py-2.5 text-xs font-medium text-brand-blue">
                                <CheckCircle2 className="w-3.5 h-3.5" />
                                {t("dl.alreadySubscribed")}
                              </div>
                            ) : (
                              <>
                                <div className="flex gap-1.5">
                                  <input
                                    type="email"
                                    value={subscribeEmail}
                                    onChange={(e) => setSubscribeEmail(e.target.value)}
                                    onKeyDown={(e) => e.key === "Enter" && handleSubscribe(p.key)}
                                    placeholder={t("dl.emailPlaceholder")}
                                    disabled={subscribeStatus === "loading"}
                                    className="flex-1 min-w-0 rounded-lg border border-dark-border bg-dark-surface2 px-3 py-2 text-xs text-dark-text placeholder:text-dark-text-muted/50 focus:outline-none focus:border-brand-blue/50 disabled:opacity-50 transition-colors"
                                  />
                                  <button
                                    type="button"
                                    onClick={() => handleSubscribe(p.key)}
                                    disabled={subscribeStatus === "loading" || !subscribeEmail.trim()}
                                    className="flex-shrink-0 rounded-lg bg-brand-blue px-3 py-2 text-xs font-semibold text-white hover:bg-brand-blue/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                                  >
                                    {subscribeStatus === "loading" ? (
                                      <Loader2 className="w-3.5 h-3.5 animate-spin" />
                                    ) : (
                                      <Bell className="w-3.5 h-3.5" />
                                    )}
                                  </button>
                                </div>
                                {subscribeStatus === "error" && (
                                  <div className="flex items-center justify-center gap-1 text-[10px] text-red-400">
                                    <AlertCircle className="w-3 h-3" />
                                    {t("dl.subscribeError")}
                                  </div>
                                )}
                                <button
                                  type="button"
                                  onClick={() => { setSubscribeTarget(null); setSubscribeStatus("idle"); }}
                                  className="text-[10px] text-dark-text-muted hover:text-dark-text-secondary transition-colors"
                                >
                                  {t("dl.comingSoon")}
                                </button>
                              </>
                            )}
                          </motion.div>
                        ) : (
                          <motion.button
                            key="notify-btn"
                            type="button"
                            initial={{ opacity: 0 }}
                            animate={{ opacity: 1 }}
                            exit={{ opacity: 0 }}
                            onClick={() => { setSubscribeTarget(p.key); setSubscribeStatus("idle"); setSubscribeEmail(""); }}
                            className="inline-flex items-center justify-center gap-2 w-full rounded-lg border border-dashed border-dark-text-muted/30 px-5 py-2.5 text-xs font-medium text-dark-text-muted hover:border-brand-blue/40 hover:text-brand-blue/80 transition-colors duration-200"
                          >
                            <Bell className="w-3.5 h-3.5" />
                            {t("dl.notifyMe")}
                          </motion.button>
                        )}
                      </AnimatePresence>
                    </div>
                  )}
                </motion.div>
              );
            })}
          </motion.div>

          {/* Browser Extension */}
          <motion.div
            className="max-w-4xl mx-auto mb-16"
            initial={{ opacity: 0, y: 20 }}
            whileInView={{ opacity: 1, y: 0 }}
            viewport={{ once: true }}
            transition={{ duration: 0.5, delay: 0.2 }}
          >
            <div className="relative rounded-xl border border-dark-border bg-dark-surface1 p-6 flex flex-col sm:flex-row items-center gap-5">
              <div className="flex items-center gap-4 flex-1 min-w-0">
                <div className="w-12 h-12 rounded-xl bg-gradient-to-br from-brand-blue/20 to-brand-cyan/20 border border-brand-blue/20 flex items-center justify-center flex-shrink-0">
                  <Puzzle className="w-6 h-6 text-brand-blue" />
                </div>
                <div className="min-w-0">
                  <h3 className="text-sm font-semibold text-dark-text">{t("dl.extensionTitle")}</h3>
                  <p className="text-xs text-dark-text-muted mt-0.5">{t("dl.extensionDesc")}</p>
                  {release?.assets.extension && (
                    <p className="text-[10px] text-dark-text-muted mt-1">
                      {t("dl.version", { version: release.version })}
                      <span className="ml-1.5">({formatSize(release.assets.extension.size)})</span>
                      <span className="ml-1.5">· Chrome + Firefox</span>
                    </p>
                  )}
                </div>
              </div>
              <div className="flex items-center gap-3 flex-shrink-0">
                {loading ? (
                  <div className="inline-flex items-center justify-center gap-2 rounded-lg bg-brand-blue/50 px-5 py-2.5 text-xs font-semibold text-white/70 cursor-wait">
                    <Loader2 className="w-3.5 h-3.5 animate-spin" />
                    {t("dl.loading")}
                  </div>
                ) : release?.assets.extension ? (
                  <a
                    href={release.assets.extension.download_url}
                    className="inline-flex items-center justify-center gap-2 rounded-lg border border-brand-blue/30 bg-brand-blue/10 px-5 py-2.5 text-xs font-semibold text-brand-blue hover:bg-brand-blue/20 transition-colors"
                  >
                    <Download className="w-3.5 h-3.5" />
                    {t("dl.downloadExtension")}
                  </a>
                ) : (
                  <div className="inline-flex items-center justify-center gap-2 rounded-lg border border-dark-border px-5 py-2.5 text-xs font-medium text-dark-text-muted">
                    {t("dl.comingSoon")}
                  </div>
                )}
              </div>
            </div>
          </motion.div>

          {/* Tech stack + Downloads counter */}
          <motion.div
            className="flex flex-col items-center gap-4"
            initial={{ opacity: 0 }}
            whileInView={{ opacity: 1 }}
            viewport={{ once: true }}
            transition={{ duration: 0.5, delay: 0.3 }}
          >
            {release && release.total_downloads > 0 && (
              <div className="inline-flex items-center gap-2 text-sm text-dark-text-secondary">
                <TrendingUp className="w-4 h-4 text-success" />
                <span>
                  <span className="font-semibold text-dark-text">{release.total_downloads.toLocaleString()}</span>
                  {" "}{t("dl.totalDownloads")}
                </span>
              </div>
            )}
            <div className="inline-flex items-center gap-3 sm:gap-6 rounded-full border border-dark-border bg-dark-surface1/50 px-4 sm:px-6 py-2.5 sm:py-3 backdrop-blur-sm">
              {techStack.map((ts, i) => (
                <span key={ts.name}>
                  <span className={`text-[10px] sm:text-xs font-semibold ${ts.color}`}>{ts.name}</span>
                  {i < techStack.length - 1 && <span className="ml-3 sm:ml-6 inline-block h-3 sm:h-4 w-px bg-dark-border" />}
                </span>
              ))}
            </div>
          </motion.div>
        </div>
      </LampEffect>
    </section>
  );
}
