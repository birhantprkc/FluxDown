import { motion, AnimatePresence } from "framer-motion";
import { useState, useEffect, useMemo, useCallback } from "react";
import { GridBackground } from "@/components/ui/grid-background";
import { GITHUB_REPO_URL } from "@/lib/utils";
import { useLocale } from "@/lib/i18n";

/* ============================================================
   HeroSection — Interactive App Mockup with Detail Panel
   - Click sidebar / tabs to filter tasks
   - Click a task row to open the detail panel on the right
   - Click the sun/moon icon in title bar to toggle light/dark theme
   - Detail panel: 1:1 match with real FluxDown app
   ============================================================ */

// ── Theme system (matches real app's app_colors.dart) ──
interface MockupTheme {
  bg: string;
  surface1: string;
  surface2: string;
  surface3: string;
  border: string;
  hoverBg: string;
  textPrimary: string;
  textSecondary: string;
  textMuted: string;
  accent: string;
  accentBg: string;
  // shadows/glow for the outer mockup frame
  shadow: string;
  glowFrom: string;
  // grid empty cell color
  gridEmpty: string;
  // hover on sidebar (non-selected)
  sidebarHover: string;
  // task row hover
  rowHover: string;
  isDark: boolean;
}

const darkTheme: MockupTheme = {
  bg: "#0A0A0B",
  surface1: "#111113",
  surface2: "#1A1A1D",
  surface3: "#232326",
  border: "#27272A",
  hoverBg: "#1A1A1D",
  textPrimary: "#FAFAFA",
  textSecondary: "#A1A1AA",
  textMuted: "#52525B",
  accent: "#3B82F6",
  accentBg: "rgba(59,130,246,0.1)",
  shadow: "0 25px 80px rgba(59,130,246,0.12), 0 0 40px rgba(59,130,246,0.06)",
  glowFrom: "rgba(59,130,246,0.20)",
  gridEmpty: "#232326",
  sidebarHover: "rgba(255,255,255,0.03)",
  rowHover: "rgba(255,255,255,0.02)",
  isDark: true,
};

const lightTheme: MockupTheme = {
  bg: "#F8F9FA",
  surface1: "#FFFFFF",
  surface2: "#F1F3F5",
  surface3: "#E9ECEF",
  border: "#E4E4E7",
  hoverBg: "#F5F5F5",
  textPrimary: "#09090B",
  textSecondary: "#71717A",
  textMuted: "#A1A1AA",
  accent: "#3B82F6",
  accentBg: "rgba(59,130,246,0.08)",
  shadow: "0 25px 60px rgba(0,0,0,0.08), 0 0 30px rgba(0,0,0,0.04)",
  glowFrom: "rgba(59,130,246,0.10)",
  gridEmpty: "#E9ECEF",
  sidebarHover: "rgba(0,0,0,0.03)",
  rowHover: "rgba(0,0,0,0.02)",
  isDark: false,
};

type TaskCategory = "all" | "downloading" | "completed" | "paused" | "error";
type FileCategory =
  | "all"
  | "video"
  | "audio"
  | "document"
  | "image"
  | "archive"
  | "other";

interface SegmentData {
  index: number;
  startByte: number;
  endByte: number;
  downloadedBytes: number;
}

interface TaskData {
  id: string;
  ext: string;
  name: string;
  size: string;
  totalBytes: number;
  downloadedBytes: number;
  baseProgress: number;
  barColor: string;
  speed: string;
  speedColor: string;
  status: string;
  statusColor: string;
  statusKey: TaskCategory;
  fileCategory: FileCategory;
  animated?: boolean;
  segments: SegmentData[];
  url: string;
  saveDir: string;
  eta: string;
  errorMsg?: string;
}

// IDM-style segment colors — accent first, then cycle 15 fixed colors
const SEGMENT_COLORS = [
  "#3B82F6", // accent (blue)
  "#22C55E",
  "#F59E0B",
  "#A855F7",
  "#06B6D4",
  "#EC4899",
  "#14B8A6",
  "#EF4444",
  "#8B5CF6",
  "#F97316",
  "#10B981",
  "#E11D48",
  "#0EA5E9",
  "#D946EF",
  "#84CC16",
  "#64748B",
];
function segColor(index: number): string {
  return SEGMENT_COLORS[index % SEGMENT_COLORS.length]!;
}

// ── Task data with segments ──
// status/eta/errorMsg are now i18n keys resolved at render time
const TASKS: TaskData[] = [
  {
    id: "t1",
    ext: "zip",
    name: "4K-wallpaper-collection.zip",
    size: "847.2 MB",
    totalBytes: 888_300_000,
    downloadedBytes: 597_823_900,
    baseProgress: 67.3,
    barColor: "#F59E0B",
    speed: "---",
    speedColor: "#52525B",
    status: "mockup.statusPaused",
    statusColor: "#F59E0B",
    statusKey: "paused",
    fileCategory: "archive",
    url: "https://cdn.example.com/4K-wallpaper-collection.zip",
    saveDir: "D:\\Downloads",
    eta: "---",
    segments: [
      {
        index: 0,
        startByte: 0,
        endByte: 222_074_999,
        downloadedBytes: 222_074_999,
      },
      {
        index: 1,
        startByte: 222_075_000,
        endByte: 444_149_999,
        downloadedBytes: 195_000_000,
      },
      {
        index: 2,
        startByte: 444_150_000,
        endByte: 666_224_999,
        downloadedBytes: 120_748_900,
      },
      {
        index: 3,
        startByte: 666_225_000,
        endByte: 888_299_999,
        downloadedBytes: 60_000_000,
      },
    ],
  },
  {
    id: "t2",
    ext: "mp4",
    name: "React-Advanced-Tutorial.mp4",
    size: "2.1 GB",
    totalBytes: 2_254_857_830,
    downloadedBytes: 1_657_320_506,
    baseProgress: 73.5,
    barColor: "#3B82F6",
    speed: "45.2 MB/s",
    speedColor: "#22C55E",
    status: "mockup.statusDownloading",
    statusColor: "#3B82F6",
    statusKey: "downloading",
    fileCategory: "video",
    animated: true,
    url: "https://media.example.com/React-Advanced-Tutorial.mp4",
    saveDir: "D:\\Downloads",
    eta: "mockup.eta:13",
    segments: [
      {
        index: 0,
        startByte: 0,
        endByte: 281_857_228,
        downloadedBytes: 281_857_228,
      },
      {
        index: 1,
        startByte: 281_857_229,
        endByte: 563_714_457,
        downloadedBytes: 281_857_228,
      },
      {
        index: 2,
        startByte: 563_714_458,
        endByte: 845_571_686,
        downloadedBytes: 281_857_228,
      },
      {
        index: 3,
        startByte: 845_571_687,
        endByte: 1_127_428_915,
        downloadedBytes: 245_000_000,
      },
      {
        index: 4,
        startByte: 1_127_428_916,
        endByte: 1_409_286_144,
        downloadedBytes: 200_000_000,
      },
      {
        index: 5,
        startByte: 1_409_286_145,
        endByte: 1_691_143_373,
        downloadedBytes: 180_000_000,
      },
      {
        index: 6,
        startByte: 1_691_143_374,
        endByte: 1_973_000_602,
        downloadedBytes: 120_000_000,
      },
      {
        index: 7,
        startByte: 1_973_000_603,
        endByte: 2_254_857_830,
        downloadedBytes: 66_748_822,
      },
    ],
  },
  {
    id: "t3",
    ext: "pdf",
    name: "annual-report-2025.pdf",
    size: "24.6 MB",
    totalBytes: 25_795_276,
    downloadedBytes: 25_795_276,
    baseProgress: 100,
    barColor: "#22C55E",
    speed: "---",
    speedColor: "#52525B",
    status: "mockup.statusCompleted",
    statusColor: "#22C55E",
    statusKey: "completed",
    fileCategory: "document",
    url: "https://reports.example.com/annual-report-2025.pdf",
    saveDir: "D:\\Downloads",
    eta: "---",
    segments: [
      {
        index: 0,
        startByte: 0,
        endByte: 12_897_637,
        downloadedBytes: 12_897_637,
      },
      {
        index: 1,
        startByte: 12_897_638,
        endByte: 25_795_275,
        downloadedBytes: 12_897_638,
      },
    ],
  },
  {
    id: "t4",
    ext: "gz",
    name: "project-v2.0-src.tar.gz",
    size: "312.4 MB",
    totalBytes: 327_580_000,
    downloadedBytes: 147_738_580,
    baseProgress: 45.1,
    barColor: "#3B82F6",
    speed: "28.7 MB/s",
    speedColor: "#22C55E",
    status: "mockup.statusDownloading",
    statusColor: "#3B82F6",
    statusKey: "downloading",
    fileCategory: "archive",
    animated: true,
    url: "https://releases.example.com/project-v2.0-src.tar.gz",
    saveDir: "D:\\Downloads",
    eta: "mockup.eta:6",
    segments: [
      {
        index: 0,
        startByte: 0,
        endByte: 81_894_999,
        downloadedBytes: 81_895_000,
      },
      {
        index: 1,
        startByte: 81_895_000,
        endByte: 163_789_999,
        downloadedBytes: 45_000_000,
      },
      {
        index: 2,
        startByte: 163_790_000,
        endByte: 245_684_999,
        downloadedBytes: 15_843_580,
      },
      {
        index: 3,
        startByte: 245_685_000,
        endByte: 327_579_999,
        downloadedBytes: 5_000_000,
      },
    ],
  },
  {
    id: "t5",
    ext: "exe",
    name: "system-driver-update.exe",
    size: "89.3 MB",
    totalBytes: 93_633_536,
    downloadedBytes: 11_236_024,
    baseProgress: 12,
    barColor: "#EF4444",
    speed: "---",
    speedColor: "#52525B",
    status: "mockup.statusError",
    statusColor: "#EF4444",
    statusKey: "error",
    fileCategory: "other",
    url: "https://drivers.example.com/system-driver-update.exe",
    saveDir: "D:\\Downloads",
    eta: "---",
    errorMsg: "mockup.errorTimeout",
    segments: [
      {
        index: 0,
        startByte: 0,
        endByte: 46_816_767,
        downloadedBytes: 11_236_024,
      },
      {
        index: 1,
        startByte: 46_816_768,
        endByte: 93_633_535,
        downloadedBytes: 0,
      },
    ],
  },
];

/* ── Icons ── */
const gridIcon = (
  <>
    <rect x="3" y="3" width="7" height="7" />
    <rect x="14" y="3" width="7" height="7" />
    <rect x="14" y="14" width="7" height="7" />
    <rect x="3" y="14" width="7" height="7" />
  </>
);
const filmIcon = (
  <>
    <rect x="2" y="2" width="20" height="20" rx="2" />
    <line x1="7" y1="2" x2="7" y2="22" />
    <line x1="17" y1="2" x2="17" y2="22" />
    <line x1="2" y1="12" x2="22" y2="12" />
  </>
);
const musicIcon = (
  <>
    <path d="M9 18V5l12-2v13" />
    <circle cx="6" cy="18" r="3" />
    <circle cx="18" cy="16" r="3" />
  </>
);
const fileTextIcon = (
  <>
    <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
    <polyline points="14 2 14 8 20 8" />
    <line x1="16" y1="13" x2="8" y2="13" />
    <line x1="16" y1="17" x2="8" y2="17" />
  </>
);
const imageIcon = (
  <>
    <rect x="3" y="3" width="18" height="18" rx="2" />
    <circle cx="8.5" cy="8.5" r="1.5" />
    <polyline points="21 15 16 10 5 21" />
  </>
);
const archiveIcon = (
  <>
    <polyline points="21 8 21 21 3 21 3 8" />
    <rect x="1" y="3" width="22" height="5" />
    <line x1="10" y1="12" x2="14" y2="12" />
  </>
);
const fileIcon = (
  <>
    <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
    <polyline points="14 2 14 8 20 8" />
  </>
);

const SIDEBAR_ICONS: {
  icon: React.ReactNode;
  labelKey: keyof import("@/lib/locales").Messages;
  key: FileCategory;
}[] = [
  { icon: gridIcon, labelKey: "mockup.allFiles", key: "all" },
  { icon: filmIcon, labelKey: "mockup.video", key: "video" },
  { icon: musicIcon, labelKey: "mockup.audio", key: "audio" },
  { icon: fileTextIcon, labelKey: "mockup.document", key: "document" },
  { icon: imageIcon, labelKey: "mockup.image", key: "image" },
  { icon: archiveIcon, labelKey: "mockup.archive", key: "archive" },
  { icon: fileIcon, labelKey: "mockup.other", key: "other" },
];

const TAB_KEYS: {
  labelKey: keyof import("@/lib/locales").Messages;
  key: TaskCategory;
}[] = [
  { labelKey: "mockup.tabAll", key: "all" },
  { labelKey: "mockup.tabDownloading", key: "downloading" },
  { labelKey: "mockup.tabCompleted", key: "completed" },
  { labelKey: "mockup.tabPaused", key: "paused" },
  { labelKey: "mockup.tabError", key: "error" },
];

export default function HeroSection() {
  const [activeFile, setActiveFile] = useState<FileCategory>("all");
  const [activeTab, setActiveTab] = useState<TaskCategory>("all");
  const [selectedTask, setSelectedTask] = useState<string | null>(null);
  const [hoveredTask, setHoveredTask] = useState<string | null>(null);
  const [animOffset, setAnimOffset] = useState(0);
  const [isDark, setIsDark] = useState(true);
  const theme = isDark ? darkTheme : lightTheme;
  const { t } = useLocale();

  // Sync with global theme
  useEffect(() => {
    setIsDark(!window.__isLightTheme?.());
    const onThemeChange = (e: CustomEvent<{ light: boolean }>) => {
      setIsDark(!e.detail.light);
    };
    window.addEventListener("theme-change", onThemeChange as EventListener);
    return () =>
      window.removeEventListener(
        "theme-change",
        onThemeChange as EventListener,
      );
  }, []);

  const handleThemeToggle = useCallback(() => {
    window.__toggleTheme?.();
  }, []);

  useEffect(() => {
    const interval = setInterval(() => {
      setAnimOffset((prev) => (prev >= 20 ? 0 : prev + 0.15));
    }, 100);
    return () => clearInterval(interval);
  }, []);

  const filteredTasks = TASKS.filter((task) => {
    if (activeFile !== "all" && task.fileCategory !== activeFile) return false;
    if (activeTab !== "all" && task.statusKey !== activeTab) return false;
    return true;
  });

  const countByFile = (fc: FileCategory) =>
    fc === "all"
      ? TASKS.length
      : TASKS.filter((task) => task.fileCategory === fc).length;

  const countByTab = (tc: TaskCategory) => {
    const pool =
      activeFile === "all"
        ? TASKS
        : TASKS.filter((task) => task.fileCategory === activeFile);
    return tc === "all"
      ? pool.length
      : pool.filter((task) => task.statusKey === tc).length;
  };

  const getProgress = (task: TaskData) =>
    task.animated
      ? Math.min(task.baseProgress + animOffset, 99.9)
      : task.baseProgress;

  /** Resolve i18n-aware task status text */
  const resolveStatus = (task: TaskData) =>
    t(task.status as keyof import("@/lib/locales").Messages);

  /** Resolve i18n-aware ETA text */
  const resolveEta = (task: TaskData) => {
    if (task.eta === "---") return "---";
    // Format: "mockup.eta:13" → t("mockup.eta", { n: "13" })
    if (task.eta.startsWith("mockup.eta:")) {
      const n = task.eta.split(":")[1]!;
      return t("mockup.eta", { n });
    }
    return task.eta;
  };

  /** Resolve i18n-aware error message */
  const resolveError = (task: TaskData) =>
    task.errorMsg
      ? t(task.errorMsg as keyof import("@/lib/locales").Messages)
      : undefined;

  const getSubtitle = (task: TaskData) => {
    if (task.animated) return `HTTP · ${task.size} · ${task.speed}`;
    if (task.statusKey === "paused")
      return `HTTP · ${task.size} · ${t("mockup.subtitlePaused")}`;
    if (task.statusKey === "error")
      return `HTTP · ${task.size} · ${t("mockup.subtitleTimeout")}`;
    return `HTTP · ${task.size}`;
  };

  const selectedTaskData = selectedTask
    ? (TASKS.find((task) => task.id === selectedTask) ?? null)
    : null;

  return (
    <section className="relative min-h-screen w-full overflow-hidden">
      <GridBackground className="absolute inset-0 -z-10" />
      <div className="absolute top-1/4 left-1/4 w-[300px] h-[300px] sm:w-[600px] sm:h-[600px] rounded-full bg-[#38bdf8]/5 blur-[80px] sm:blur-[128px] animate-pulse -z-10" />
      <div className="absolute bottom-1/4 right-1/4 w-[250px] h-[250px] sm:w-[500px] sm:h-[500px] rounded-full bg-[#06b6d4]/5 blur-[80px] sm:blur-[128px] animate-pulse [animation-delay:1s] -z-10" />

      <div className="relative z-10 mx-auto max-w-7xl px-4 sm:px-6 pt-24 sm:pt-32 pb-16 sm:pb-20">
        {/* Badge */}
        <motion.div
          initial={{ opacity: 0, y: 20 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.5 }}
          className="flex justify-center mb-8"
        >
          <span className="inline-flex items-center gap-2 rounded-full border border-dark-border bg-dark-surface1/50 px-4 py-1.5 text-xs font-medium text-dark-text-secondary backdrop-blur-sm">
            <span className="relative flex h-2 w-2">
              <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-[#22C55E] opacity-75" />
              <span className="relative inline-flex h-2 w-2 rounded-full bg-[#22C55E]" />
            </span>
            {t("hero.badge")}
          </span>
        </motion.div>

        <motion.h1
          initial={{ opacity: 0, y: 30 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.6, delay: 0.1 }}
          className="text-center text-5xl sm:text-6xl lg:text-7xl font-bold tracking-tight leading-[1.1]"
        >
          <span className="block text-dark-text">{t("hero.title1")}</span>
          <span className="block bg-gradient-to-r from-[#38bdf8] to-[#06b6d4] bg-clip-text text-transparent">
            {t("hero.title2")}
          </span>
        </motion.h1>

        <motion.p
          initial={{ opacity: 0, y: 30 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.6, delay: 0.2 }}
          className="mt-6 text-center text-lg sm:text-xl text-dark-text-secondary max-w-2xl mx-auto leading-relaxed"
        >
          {t("hero.subtitle")}
        </motion.p>

        <motion.div
          initial={{ opacity: 0, y: 30 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.6, delay: 0.3 }}
          className="mt-10 flex flex-wrap items-center justify-center gap-3"
        >
          <a
            href="#download"
            className="group inline-flex items-center gap-2.5 rounded-xl bg-[#3B82F6] px-8 py-3.5 text-sm font-semibold text-white shadow-lg shadow-[#3B82F6]/25 hover:shadow-[#3B82F6]/40 hover:bg-[#3B82F6]/90 transition-all duration-300"
          >
            <svg
              className="h-4 w-4 group-hover:-translate-y-0.5 transition-transform"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
              <polyline points="7 10 12 15 17 10" />
              <line x1="12" y1="15" x2="12" y2="3" />
            </svg>
            {t("hero.cta")}
          </a>
          <a
            href={GITHUB_REPO_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="group inline-flex items-center gap-2.5 rounded-xl border border-dark-border bg-dark-surface1/60 px-8 py-3.5 text-sm font-semibold text-dark-text backdrop-blur-sm hover:border-dark-text-muted/50 hover:bg-dark-surface2 transition-all duration-300"
          >
            <svg
              className="h-4 w-4 text-dark-text-secondary group-hover:text-dark-text transition-colors"
              viewBox="0 0 24 24"
              fill="currentColor"
            >
              <path d="M12 .5C5.65.5.5 5.65.5 12c0 5.08 3.29 9.39 7.86 10.91.58.11.79-.25.79-.55 0-.27-.01-1.17-.02-2.12-3.2.7-3.87-1.36-3.87-1.36-.52-1.33-1.28-1.68-1.28-1.68-1.04-.71.08-.7.08-.7 1.15.08 1.76 1.19 1.76 1.19 1.03 1.76 2.69 1.25 3.35.96.1-.75.4-1.25.72-1.54-2.55-.29-5.24-1.28-5.24-5.68 0-1.26.45-2.28 1.19-3.09-.12-.29-.51-1.46.11-3.05 0 0 .97-.31 3.17 1.18a11.04 11.04 0 0 1 5.78 0c2.2-1.49 3.17-1.18 3.17-1.18.62 1.59.23 2.76.11 3.05.74.81 1.19 1.83 1.19 3.09 0 4.41-2.69 5.38-5.26 5.67.41.35.77 1.05.77 2.12 0 1.53-.01 2.76-.01 3.14 0 .3.21.67.8.55A11.51 11.51 0 0 0 23.5 12C23.5 5.65 18.35.5 12 .5z" />
            </svg>
            {t("hero.github")}
          </a>
        </motion.div>

        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          transition={{ duration: 0.8, delay: 0.5 }}
          className="mt-16 flex items-center justify-center gap-6 sm:gap-16"
        >
          <StatItem
            value={t("hero.stat1.value")}
            label={t("hero.stat1.label")}
          />
          <div className="h-8 w-px bg-dark-border" />
          <StatItem
            value={t("hero.stat2.value")}
            label={t("hero.stat2.label")}
          />
          <div className="h-8 w-px bg-dark-border hidden sm:block" />
          <div className="hidden sm:block">
            <StatItem
              value={t("hero.stat3.value")}
              label={t("hero.stat3.label")}
            />
          </div>
        </motion.div>

        {/* ===== Interactive App Mockup ===== */}
        <motion.div
          initial={{ opacity: 0, y: 60 }}
          animate={{
            opacity: 1,
            y: 0,
            maxWidth: selectedTaskData ? 1360 : 1024,
          }}
          transition={{ duration: 0.35, ease: [0.22, 1, 0.36, 1] }}
          className="mt-12 sm:mt-20 relative mx-auto"
          style={{ width: "100%" }}
        >
          <div
            className="absolute -inset-4 rounded-2xl blur-2xl opacity-60"
            style={{
              background: `linear-gradient(to bottom, ${theme.glowFrom}, ${theme.glowFrom}33, transparent)`,
            }}
          />

          <div
            className="relative overflow-hidden select-none"
            style={{
              borderRadius: "12px",
              border: `1px solid ${theme.border}`,
              backgroundColor: theme.surface1,
              boxShadow: theme.shadow,
              transition:
                "background-color 0.3s, border-color 0.3s, box-shadow 0.3s",
            }}
          >
            {/* ── Title Bar ── */}
            <div
              className="flex items-center justify-between"
              style={{
                height: "36px",
                backgroundColor: theme.surface1,
                borderBottom: `1px solid ${theme.border}`,
                transition: "background-color 0.3s, border-color 0.3s",
              }}
            >
              <div
                className="flex items-center pl-3 sm:pl-4"
                style={{ gap: "6px" }}
              >
                <img
                  src="/logo.svg"
                  alt=""
                  style={{ width: "18px", height: "18px", borderRadius: "4px" }}
                />
                <span style={{ letterSpacing: "0.3px" }}>
                  <span
                    style={{
                      fontSize: "12px",
                      fontWeight: 600,
                      color: theme.accent,
                    }}
                  >
                    Flux
                  </span>
                  <span
                    style={{
                      fontSize: "12px",
                      fontWeight: 500,
                      color: theme.textPrimary,
                      transition: "color 0.3s",
                    }}
                  >
                    Down
                  </span>
                </span>
              </div>
              <div className="flex items-center">
                {/* Action buttons — hidden on small screens */}
                {[
                  <>
                    <rect x="6" y="4" width="4" height="16" rx="1" />
                    <rect x="14" y="4" width="4" height="16" rx="1" />
                  </>,
                  <polygon points="6 3 20 12 6 21" />,
                  <>
                    <circle cx="12" cy="12" r="3" />
                    <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
                  </>,
                ].map((icon, i) => (
                  <div
                    key={i}
                    className="hidden sm:flex items-center justify-center transition-colors"
                    style={{
                      width: "32px",
                      height: "36px",
                      cursor: "pointer",
                      backgroundColor: "transparent",
                    }}
                    onMouseEnter={(e) => {
                      e.currentTarget.style.backgroundColor =
                        theme.sidebarHover;
                    }}
                    onMouseLeave={(e) => {
                      e.currentTarget.style.backgroundColor = "transparent";
                    }}
                  >
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke={theme.textSecondary}
                      strokeWidth="2"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    >
                      {icon}
                    </svg>
                  </div>
                ))}
                {/* Theme toggle */}
                <div
                  onClick={handleThemeToggle}
                  className="flex items-center justify-center transition-colors"
                  style={{
                    width: "32px",
                    height: "36px",
                    cursor: "pointer",
                    backgroundColor: "transparent",
                  }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.backgroundColor = theme.sidebarHover;
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.backgroundColor = "transparent";
                  }}
                >
                  <svg
                    width="14"
                    height="14"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke={theme.textSecondary}
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  >
                    {isDark ? (
                      <>
                        <circle cx="12" cy="12" r="5" />
                        <line x1="12" y1="1" x2="12" y2="3" />
                        <line x1="12" y1="21" x2="12" y2="23" />
                        <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
                        <line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
                        <line x1="1" y1="12" x2="3" y2="12" />
                        <line x1="21" y1="12" x2="23" y2="12" />
                        <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
                        <line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
                      </>
                    ) : (
                      <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
                    )}
                  </svg>
                </div>
                <div
                  className="hidden sm:block"
                  style={{
                    width: "1px",
                    height: "16px",
                    backgroundColor: theme.border,
                    margin: "0 2px",
                    transition: "background-color 0.3s",
                  }}
                />
                <div
                  className="hidden sm:flex items-center justify-center transition-colors"
                  style={{
                    width: "32px",
                    height: "36px",
                    cursor: "pointer",
                    backgroundColor: "transparent",
                  }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.backgroundColor = theme.sidebarHover;
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.backgroundColor = "transparent";
                  }}
                >
                  <svg
                    width="12"
                    height="12"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke={theme.textSecondary}
                    strokeWidth="2"
                    strokeLinecap="round"
                  >
                    <line x1="5" y1="12" x2="19" y2="12" />
                  </svg>
                </div>
                <div
                  className="hidden sm:flex items-center justify-center transition-colors"
                  style={{
                    width: "32px",
                    height: "36px",
                    cursor: "pointer",
                    backgroundColor: "transparent",
                  }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.backgroundColor = theme.sidebarHover;
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.backgroundColor = "transparent";
                  }}
                >
                  <svg
                    width="12"
                    height="12"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke={theme.textSecondary}
                    strokeWidth="2"
                    strokeLinecap="round"
                  >
                    <rect x="3" y="3" width="18" height="18" rx="2" />
                  </svg>
                </div>
                <div
                  className="flex items-center justify-center hover:bg-[#EF4444]/10 transition-colors"
                  style={{ width: "32px", height: "36px", cursor: "pointer" }}
                >
                  <svg
                    width="12"
                    height="12"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke={theme.textSecondary}
                    strokeWidth="2"
                    strokeLinecap="round"
                  >
                    <line x1="18" y1="6" x2="6" y2="18" />
                    <line x1="6" y1="6" x2="18" y2="18" />
                  </svg>
                </div>
              </div>
            </div>

            {/* ── Main Area: Sidebar + Content + Detail Panel ── */}
            <motion.div
              className="flex"
              animate={{ height: selectedTaskData ? 540 : 340 }}
              transition={{ duration: 0.3, ease: [0.22, 1, 0.36, 1] }}
              style={{ minHeight: 260 }}
            >
              {/* Sidebar */}
              <div
                className="flex-col shrink-0 hidden md:flex"
                style={{
                  width: "200px",
                  backgroundColor: theme.surface1,
                  borderRight: `1px solid ${theme.border}`,
                  transition: "background-color 0.3s, border-color 0.3s",
                }}
              >
                <div
                  style={{
                    padding: "4px 16px",
                    fontSize: "10.5px",
                    fontWeight: 500,
                    color: theme.textMuted,
                    letterSpacing: "0.5px",
                    marginTop: "4px",
                    transition: "color 0.3s",
                  }}
                >
                  {t("mockup.category")}
                </div>
                <nav className="flex-1 mt-1">
                  {SIDEBAR_ICONS.map((item) => (
                    <SidebarItem
                      key={item.key}
                      icon={item.icon}
                      label={t(item.labelKey)}
                      count={String(countByFile(item.key))}
                      selected={activeFile === item.key}
                      onClick={() => {
                        setActiveFile(item.key);
                        setActiveTab("all");
                        setSelectedTask(null);
                      }}
                      theme={theme}
                    />
                  ))}
                </nav>
                <div
                  className="flex items-center"
                  style={{
                    padding: "16px",
                    borderTop: `1px solid ${theme.border}`,
                    transition: "border-color 0.3s",
                  }}
                >
                  <svg
                    width="11"
                    height="11"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="#22C55E"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  >
                    <line x1="12" y1="5" x2="12" y2="19" />
                    <polyline points="19 12 12 19 5 12" />
                  </svg>
                  <span
                    style={{
                      fontSize: "11px",
                      color: theme.textSecondary,
                      marginLeft: "4px",
                      transition: "color 0.3s",
                    }}
                  >
                    {t("mockup.download")}
                  </span>
                  <span
                    style={{
                      fontSize: "11px",
                      color: "#22C55E",
                      marginLeft: "6px",
                      fontVariantNumeric: "tabular-nums",
                    }}
                  >
                    12.8 MB/s
                  </span>
                </div>
              </div>

              {/* Content area */}
              <div
                className="flex-1 flex flex-col"
                style={{
                  backgroundColor: theme.bg,
                  transition: "background-color 0.3s",
                }}
              >
                <div
                  className="flex items-center overflow-x-auto scrollbar-none"
                  style={{
                    height: "34px",
                    padding: "0 12px",
                    borderBottom: `1px solid ${theme.border}`,
                    transition: "border-color 0.3s",
                  }}
                >
                  {TAB_KEYS.map((tab) => (
                    <TabItem
                      key={tab.key}
                      label={`${t(tab.labelKey)} (${countByTab(tab.key)})`}
                      active={activeTab === tab.key}
                      onClick={() => {
                        setActiveTab(tab.key);
                        setSelectedTask(null);
                      }}
                      theme={theme}
                    />
                  ))}
                </div>
                <div
                  className="flex items-center"
                  style={{
                    height: "30px",
                    padding: "0 10px",
                    backgroundColor: theme.surface1,
                    borderBottom: `1px solid ${theme.border}`,
                    transition: "background-color 0.3s, border-color 0.3s",
                  }}
                >
                  <div
                    className="flex-1 min-w-0"
                    style={{
                      fontSize: "10px",
                      fontWeight: 500,
                      color: theme.textMuted,
                      transition: "color 0.3s",
                    }}
                  >
                    {t("mockup.colFilename")}
                  </div>
                  <div
                    className="shrink-0"
                    style={{
                      width: "120px",
                      fontSize: "10px",
                      fontWeight: 500,
                      color: theme.textMuted,
                      textAlign: "center",
                      transition: "color 0.3s",
                    }}
                  >
                    {t("mockup.colProgress")}
                  </div>
                  <div
                    className="hidden sm:block shrink-0"
                    style={{
                      width: "80px",
                      fontSize: "10px",
                      fontWeight: 500,
                      color: theme.textMuted,
                      textAlign: "center",
                      transition: "color 0.3s",
                    }}
                  >
                    {t("mockup.colSpeed")}
                  </div>
                  <div
                    className="hidden sm:block shrink-0"
                    style={{
                      width: "56px",
                      fontSize: "10px",
                      fontWeight: 500,
                      color: theme.textMuted,
                      textAlign: "right",
                      transition: "color 0.3s",
                    }}
                  >
                    {t("mockup.colStatus")}
                  </div>
                </div>
                <div className="flex-1 overflow-hidden">
                  <AnimatePresence mode="popLayout">
                    {filteredTasks.map((task, i) => (
                      <motion.div
                        key={task.id}
                        layout
                        initial={{ opacity: 0, y: 10 }}
                        animate={{ opacity: 1, y: 0 }}
                        exit={{ opacity: 0, y: -10 }}
                        transition={{ duration: 0.2, delay: i * 0.02 }}
                      >
                        <TaskRow
                          task={task}
                          progress={getProgress(task)}
                          subtitle={getSubtitle(task)}
                          statusText={resolveStatus(task)}
                          selected={selectedTask === task.id}
                          hovered={hoveredTask === task.id}
                          onClick={() =>
                            setSelectedTask(
                              selectedTask === task.id ? null : task.id,
                            )
                          }
                          onHover={(h) => setHoveredTask(h ? task.id : null)}
                          theme={theme}
                        />
                      </motion.div>
                    ))}
                  </AnimatePresence>
                  {filteredTasks.length === 0 && (
                    <div className="flex items-center justify-center h-full">
                      <span
                        style={{
                          fontSize: "13px",
                          color: theme.textMuted,
                          transition: "color 0.3s",
                        }}
                      >
                        {t("mockup.noTasks")}
                      </span>
                    </div>
                  )}
                </div>
                <div
                  className="flex items-center"
                  style={{
                    height: "24px",
                    padding: "0 10px",
                    backgroundColor: theme.surface1,
                    borderTop: `1px solid ${theme.border}`,
                    gap: "12px",
                    transition: "background-color 0.3s, border-color 0.3s",
                  }}
                >
                  <div className="flex items-center" style={{ gap: "4px" }}>
                    <div
                      style={{
                        width: "6px",
                        height: "6px",
                        borderRadius: "50%",
                        backgroundColor: "#22C55E",
                      }}
                    />
                    <span
                      style={{
                        fontSize: "9.5px",
                        color: theme.textMuted,
                        transition: "color 0.3s",
                      }}
                    >
                      {t("mockup.downloading")}
                    </span>
                  </div>
                  <div className="flex items-center" style={{ gap: "3px" }}>
                    <svg
                      width="9"
                      height="9"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="#22C55E"
                      strokeWidth="2"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    >
                      <line x1="12" y1="5" x2="12" y2="19" />
                      <polyline points="19 12 12 19 5 12" />
                    </svg>
                    <span
                      style={{
                        fontSize: "9.5px",
                        color: theme.textMuted,
                        fontVariantNumeric: "tabular-nums",
                        transition: "color 0.3s",
                      }}
                    >
                      12.8 MB/s
                    </span>
                  </div>
                  <span
                    className="hidden sm:inline"
                    style={{
                      fontSize: "9.5px",
                      color: theme.textMuted,
                      transition: "color 0.3s",
                    }}
                  >
                    {t("mockup.statusActive", { n: "2", p: "1", t: "5" })}
                  </span>
                </div>
              </div>

              {/* ── Detail Panel (right side) ── */}
              <AnimatePresence>
                {selectedTaskData && (
                  <motion.div
                    initial={{ width: 0, opacity: 0 }}
                    animate={{ width: 320, opacity: 1 }}
                    exit={{ width: 0, opacity: 0 }}
                    transition={{ duration: 0.3, ease: [0.22, 1, 0.36, 1] }}
                    className="shrink-0 hidden lg:flex flex-col overflow-hidden"
                    style={{
                      backgroundColor: theme.surface1,
                      borderLeft: `1px solid ${theme.border}`,
                      transition: "background-color 0.3s, border-color 0.3s",
                    }}
                  >
                    <DetailPanel
                      task={selectedTaskData}
                      animOffset={animOffset}
                      onClose={() => setSelectedTask(null)}
                      theme={theme}
                      resolveStatus={resolveStatus}
                      resolveEta={resolveEta}
                      resolveError={resolveError}
                      t={t}
                    />
                  </motion.div>
                )}
              </AnimatePresence>
            </motion.div>
          </div>
        </motion.div>
      </div>
    </section>
  );
}

/* ================================================================
   DetailPanel — 1:1 match with real FluxDown detail_panel.dart
   ================================================================ */

function DetailPanel({
  task,
  animOffset,
  onClose,
  theme,
  resolveStatus,
  resolveEta,
  resolveError,
  t,
}: {
  task: TaskData;
  animOffset: number;
  onClose: () => void;
  theme: MockupTheme;
  resolveStatus: (task: TaskData) => string;
  resolveEta: (task: TaskData) => string;
  resolveError: (task: TaskData) => string | undefined;
  t: (
    key: keyof import("@/lib/locales").Messages,
    params?: Record<string, string>,
  ) => string;
}) {
  const progress = task.animated
    ? Math.min(task.baseProgress + animOffset, 99.9)
    : task.baseProgress;
  const dlBytes = task.animated
    ? Math.min(
        task.downloadedBytes + Math.round((animOffset * task.totalBytes) / 100),
        task.totalBytes,
      )
    : task.downloadedBytes;

  return (
    <div className="flex flex-col h-full" style={{ width: 320 }}>
      {/* Header */}
      <div
        className="flex items-center justify-between shrink-0"
        style={{
          height: "42px",
          padding: "0 12px",
          borderBottom: `1px solid ${theme.border}`,
          transition: "border-color 0.3s",
        }}
      >
        <span
          style={{
            fontSize: "13px",
            fontWeight: 600,
            color: theme.textPrimary,
            transition: "color 0.3s",
          }}
        >
          {t("mockup.detail")}
        </span>
        <div
          onClick={onClose}
          className="flex items-center justify-center rounded transition-colors cursor-pointer"
          style={{
            width: "28px",
            height: "28px",
            backgroundColor: "transparent",
          }}
          onMouseEnter={(e) => {
            e.currentTarget.style.backgroundColor = theme.sidebarHover;
          }}
          onMouseLeave={(e) => {
            e.currentTarget.style.backgroundColor = "transparent";
          }}
        >
          <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke={theme.textMuted}
            strokeWidth="2"
            strokeLinecap="round"
          >
            <line x1="18" y1="6" x2="6" y2="18" />
            <line x1="6" y1="6" x2="18" y2="18" />
          </svg>
        </div>
      </div>

      {/* Scrollable content */}
      <div className="flex-1 overflow-y-auto" style={{ padding: "16px" }}>
        {/* File info */}
        <div
          className="flex items-center"
          style={{ gap: "12px", marginBottom: "20px" }}
        >
          <div
            className="flex items-center justify-center shrink-0"
            style={{
              width: "40px",
              height: "40px",
              backgroundColor: theme.surface2,
              borderRadius: "8px",
              fontSize: "11px",
              fontWeight: 600,
              color: theme.textSecondary,
              transition: "background-color 0.3s, color 0.3s",
            }}
          >
            {task.ext}
          </div>
          <div
            style={{
              fontSize: "13px",
              color: theme.textPrimary,
              overflow: "hidden",
              display: "-webkit-box",
              WebkitLineClamp: 2,
              WebkitBoxOrient: "vertical",
              transition: "color 0.3s",
            }}
          >
            {task.name}
          </div>
        </div>

        {/* Progress % */}
        <div
          style={{
            fontSize: "26px",
            fontWeight: 600,
            color: theme.textPrimary,
            fontVariantNumeric: "tabular-nums",
            marginBottom: "8px",
            transition: "color 0.3s",
          }}
        >
          {progress.toFixed(1)}%
        </div>

        {/* Segmented progress bar */}
        <SegmentedBar
          segments={task.segments}
          totalBytes={task.totalBytes}
          animated={task.animated}
          animOffset={animOffset}
          theme={theme}
        />

        {/* IDM Grid */}
        <div style={{ marginTop: "16px" }}>
          <div
            style={{
              fontSize: "11px",
              fontWeight: 500,
              color: theme.textMuted,
              marginBottom: "8px",
              transition: "color 0.3s",
            }}
          >
            {t("mockup.distLabel")}
          </div>
          <SegmentGrid
            segments={task.segments}
            totalBytes={task.totalBytes}
            animated={task.animated}
            animOffset={animOffset}
            theme={theme}
          />
        </div>

        {/* Segment legend */}
        {task.segments.length > 1 && (
          <div
            className="flex flex-wrap"
            style={{ gap: "8px 12px", marginTop: "12px" }}
          >
            {task.segments.map((seg) => {
              const segSize = seg.endByte - seg.startByte + 1;
              const segProgress = Math.min(
                ((seg.downloadedBytes +
                  (task.animated ? (animOffset * segSize) / 100 : 0)) /
                  segSize) *
                  100,
                100,
              );
              return (
                <div
                  key={seg.index}
                  className="flex items-center"
                  style={{ gap: "4px" }}
                >
                  <div
                    style={{
                      width: "8px",
                      height: "8px",
                      borderRadius: "2px",
                      backgroundColor: segColor(seg.index),
                    }}
                  />
                  <span
                    style={{
                      fontSize: "10px",
                      color: theme.textMuted,
                      fontVariantNumeric: "tabular-nums",
                      transition: "color 0.3s",
                    }}
                  >
                    #{seg.index + 1} {segProgress.toFixed(0)}%
                  </span>
                </div>
              );
            })}
          </div>
        )}

        {/* Info table */}
        <div style={{ marginTop: "20px" }}>
          <InfoRow label={t("mockup.labelSize")} value={task.size} />
          <InfoRow
            label={t("mockup.labelDownloaded")}
            value={formatBytes(dlBytes)}
          />
          <InfoRow label={t("mockup.labelSpeed")} value={task.speed} />
          <InfoRow
            label={t("mockup.labelRemaining")}
            value={resolveEta(task)}
          />
          <InfoRow
            label={t("mockup.labelStatus")}
            value={resolveStatus(task)}
            valueColor={task.statusColor}
          />
          <InfoRow
            label={t("mockup.labelThreads")}
            value={t("mockup.threadsValue", {
              n: String(task.segments.length),
            })}
          />
          <InfoRow label={t("mockup.labelPath")} value={task.saveDir} />
          <InfoRow label={t("mockup.labelUrl")} value={task.url} mono />
          {resolveError(task) && (
            <InfoRow
              label={t("mockup.labelError")}
              value={resolveError(task)!}
              valueColor="#EF4444"
            />
          )}
        </div>
      </div>

      {/* Action buttons */}
      <div
        style={{
          padding: "16px",
          borderTop: `1px solid ${theme.border}`,
          transition: "border-color 0.3s",
        }}
      >
        {task.statusKey === "downloading" && (
          <div
            className="w-full flex items-center justify-center rounded-md cursor-pointer hover:opacity-90 transition-opacity"
            style={{
              height: "36px",
              backgroundColor: "#3B82F6",
              marginBottom: "8px",
            }}
          >
            <span style={{ fontSize: "13px", fontWeight: 500, color: "#fff" }}>
              {t("mockup.btnPause")}
            </span>
          </div>
        )}
        {(task.statusKey === "paused" || task.statusKey === "error") && (
          <div
            className="w-full flex items-center justify-center rounded-md cursor-pointer hover:opacity-90 transition-opacity"
            style={{
              height: "36px",
              backgroundColor: "#3B82F6",
              marginBottom: "8px",
            }}
          >
            <span style={{ fontSize: "13px", fontWeight: 500, color: "#fff" }}>
              {t("mockup.btnResume")}
            </span>
          </div>
        )}
        <div
          className="w-full flex items-center justify-center rounded-md cursor-pointer hover:opacity-90 transition-opacity"
          style={{ height: "36px", backgroundColor: "#EF4444" }}
        >
          <span style={{ fontSize: "13px", color: "#fff" }}>
            {t("mockup.btnDelete")}
          </span>
        </div>
      </div>
    </div>
  );
}

/* ── Segmented Progress Bar ── */
function SegmentedBar({
  segments,
  totalBytes,
  animated,
  animOffset,
  theme,
}: {
  segments: SegmentData[];
  totalBytes: number;
  animated?: boolean;
  animOffset: number;
  theme: MockupTheme;
}) {
  if (totalBytes <= 0) return null;
  return (
    <div
      className="flex w-full overflow-hidden"
      style={{
        height: "6px",
        borderRadius: "3px",
        backgroundColor: theme.surface3,
        transition: "background-color 0.3s",
      }}
    >
      {segments.map((seg) => {
        const segSize = seg.endByte - seg.startByte + 1;
        const widthPct = (segSize / totalBytes) * 100;
        const dl = animated
          ? Math.min(
              seg.downloadedBytes + (animOffset * segSize) / 100,
              segSize,
            )
          : seg.downloadedBytes;
        const fillPct = Math.min((dl / segSize) * 100, 100);
        return (
          <div
            key={seg.index}
            style={{
              width: `${widthPct}%`,
              height: "100%",
              position: "relative",
            }}
          >
            <motion.div
              style={{ height: "100%", backgroundColor: segColor(seg.index) }}
              animate={{ width: `${fillPct}%` }}
              transition={{ duration: 0.3, ease: "linear" }}
            />
          </div>
        );
      })}
    </div>
  );
}

/* ── IDM-style Segment Grid ── */
function SegmentGrid({
  segments,
  totalBytes,
  animated,
  animOffset,
  theme,
}: {
  segments: SegmentData[];
  totalBytes: number;
  animated?: boolean;
  animOffset: number;
  theme: MockupTheme;
}) {
  const cols = 42;
  const rows = Math.max(8, Math.min(16, segments.length * 3 + 4));
  const totalCells = cols * rows;
  const cellSize = 5;
  const cellGap = 1.5;

  const cells = useMemo(() => {
    if (totalBytes <= 0) return [];
    const bytesPerCell = totalBytes / totalCells;
    const result: { color: string; opacity: number }[] = [];

    for (let i = 0; i < totalCells; i++) {
      const cellMid = Math.round((i + 0.5) * bytesPerCell);
      let owner: SegmentData | null = null;
      for (const seg of segments) {
        if (cellMid >= seg.startByte && cellMid <= seg.endByte) {
          owner = seg;
          break;
        }
      }
      if (!owner) {
        result.push({ color: theme.gridEmpty, opacity: 1 });
        continue;
      }
      const offsetInSeg = cellMid - owner.startByte;
      const segSize = owner.endByte - owner.startByte + 1;
      const dl = animated
        ? Math.min(
            owner.downloadedBytes + (animOffset * segSize) / 100,
            segSize,
          )
        : owner.downloadedBytes;
      const isDownloaded = offsetInSeg < dl;
      result.push({
        color: segColor(owner.index),
        opacity: isDownloaded ? 1 : 0.12,
      });
    }
    return result;
  }, [segments, totalBytes, totalCells, animated, animOffset, theme.gridEmpty]);

  const gridWidth = cols * (cellSize + cellGap) - cellGap;
  const gridHeight = rows * (cellSize + cellGap) - cellGap;

  return (
    <div
      style={{
        padding: "6px",
        backgroundColor: theme.surface2,
        borderRadius: "6px",
        border: `1px solid ${theme.border}`,
        transition: "background-color 0.3s, border-color 0.3s",
      }}
    >
      <svg
        width="100%"
        viewBox={`0 0 ${gridWidth} ${gridHeight}`}
        style={{ display: "block" }}
      >
        {cells.map((cell, i) => {
          const col = i % cols;
          const row = Math.floor(i / cols);
          return (
            <rect
              key={i}
              x={col * (cellSize + cellGap)}
              y={row * (cellSize + cellGap)}
              width={cellSize}
              height={cellSize}
              rx={1}
              fill={cell.color}
              opacity={cell.opacity}
            />
          );
        })}
      </svg>
    </div>
  );
}

/* ── Info Row ── */
function InfoRow({
  label,
  value,
  valueColor,
  mono,
}: {
  label: string;
  value: string;
  valueColor?: string;
  mono?: boolean;
}) {
  return (
    <div className="flex" style={{ marginBottom: "10px" }}>
      <div
        style={{
          width: "48px",
          fontSize: "11px",
          color: "#52525B",
          flexShrink: 0,
        }}
      >
        {label}
      </div>
      <div
        style={{
          flex: 1,
          fontSize: "11px",
          color: valueColor ?? "#A1A1AA",
          fontVariantNumeric: "tabular-nums",
          wordBreak: mono ? "break-all" : "break-word",
          lineHeight: "1.4",
        }}
      >
        {value}
      </div>
    </div>
  );
}

/* ── Helper ── */
function formatBytes(bytes: number): string {
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} B`;
}

/* ── Shared Sub-components ── */
function SidebarItem({
  icon,
  label,
  count,
  selected,
  onClick,
  theme,
}: {
  icon: React.ReactNode;
  label: string;
  count: string;
  selected?: boolean;
  onClick: () => void;
  theme: MockupTheme;
}) {
  return (
    <div
      onClick={onClick}
      className="flex items-center justify-between transition-colors duration-150 cursor-pointer"
      style={{
        height: "32px",
        margin: "1px 8px",
        padding: "0 8px",
        borderRadius: "6px",
        backgroundColor: selected ? theme.accentBg : "transparent",
      }}
      onMouseEnter={(e) => {
        if (!selected)
          e.currentTarget.style.backgroundColor = theme.sidebarHover;
      }}
      onMouseLeave={(e) => {
        if (!selected)
          e.currentTarget.style.backgroundColor = selected
            ? theme.accentBg
            : "transparent";
      }}
    >
      <div className="flex items-center" style={{ gap: "8px" }}>
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke={selected ? theme.accent : theme.textSecondary}
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          {icon}
        </svg>
        <span
          style={{
            fontSize: "12.5px",
            color: selected ? theme.accent : theme.textSecondary,
            fontWeight: selected ? 500 : 400,
            transition: "color 0.15s",
          }}
        >
          {label}
        </span>
      </div>
      <span
        style={{
          fontSize: "11px",
          color: selected ? theme.accent : theme.textMuted,
          fontVariantNumeric: "tabular-nums",
          transition: "color 0.15s",
        }}
      >
        {count}
      </span>
    </div>
  );
}

function TabItem({
  label,
  active,
  onClick,
  theme,
}: {
  label: string;
  active?: boolean;
  onClick: () => void;
  theme: MockupTheme;
}) {
  return (
    <div
      onClick={onClick}
      className="relative flex items-center cursor-pointer transition-colors shrink-0"
      style={{
        padding: "0 8px",
        height: "34px",
        backgroundColor: "transparent",
      }}
      onMouseEnter={(e) => {
        e.currentTarget.style.backgroundColor = theme.rowHover;
      }}
      onMouseLeave={(e) => {
        e.currentTarget.style.backgroundColor = "transparent";
      }}
    >
      <span
        style={{
          fontSize: "11px",
          fontWeight: active ? 500 : 400,
          color: active ? theme.textPrimary : theme.textMuted,
          transition: "color 0.15s",
          whiteSpace: "nowrap",
        }}
      >
        {label}
      </span>
      {active && (
        <motion.div
          layoutId="heroActiveTab"
          style={{
            position: "absolute",
            bottom: 0,
            left: 0,
            right: 0,
            height: "2px",
            backgroundColor: theme.accent,
          }}
          transition={{ duration: 0.25, ease: "easeInOut" }}
        />
      )}
    </div>
  );
}

function TaskRow({
  task,
  progress,
  subtitle,
  statusText,
  selected,
  hovered,
  onClick,
  onHover,
  theme,
}: {
  task: TaskData;
  progress: number;
  subtitle: string;
  statusText: string;
  selected: boolean;
  hovered: boolean;
  onClick: () => void;
  onHover: (h: boolean) => void;
  theme: MockupTheme;
}) {
  let bgColor = "transparent";
  if (selected) bgColor = theme.accentBg;
  else if (hovered) bgColor = theme.rowHover;
  return (
    <div
      onClick={onClick}
      onMouseEnter={() => onHover(true)}
      onMouseLeave={() => onHover(false)}
      className="flex items-center cursor-pointer transition-colors duration-100"
      style={{
        height: "52px",
        padding: "6px 10px",
        borderBottom: `1px solid ${theme.border}`,
        backgroundColor: bgColor,
      }}
    >
      <div className="flex items-center flex-1 min-w-0" style={{ gap: "8px" }}>
        <div
          className="flex items-center justify-center shrink-0"
          style={{
            width: "28px",
            height: "28px",
            backgroundColor: selected
              ? "rgba(59,130,246,0.15)"
              : theme.surface2,
            borderRadius: "5px",
            fontSize: "9px",
            fontWeight: 600,
            color: selected ? theme.accent : theme.textSecondary,
            transition: "all 0.15s",
          }}
        >
          {task.ext}
        </div>
        <div className="flex flex-col min-w-0">
          <div
            style={{
              fontSize: "11.5px",
              color: selected ? theme.accent : theme.textPrimary,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              transition: "color 0.15s",
            }}
          >
            {task.name}
          </div>
          <div
            style={{
              fontSize: "10px",
              color: theme.textMuted,
              marginTop: "1px",
              transition: "color 0.3s",
            }}
          >
            {subtitle}
          </div>
        </div>
      </div>
      <div
        className="flex items-center shrink-0"
        style={{ width: "120px", gap: "6px", paddingRight: "8px" }}
      >
        <div
          style={{
            flex: 1,
            height: "3px",
            backgroundColor: theme.surface3,
            borderRadius: "1.5px",
            overflow: "hidden",
            transition: "background-color 0.3s",
          }}
        >
          <motion.div
            style={{
              height: "100%",
              backgroundColor: task.barColor,
              borderRadius: "1.5px",
            }}
            animate={{ width: `${progress}%` }}
            transition={{ duration: 0.3, ease: "linear" }}
          />
        </div>
        <span
          style={{
            fontSize: "10px",
            color: theme.textSecondary,
            fontVariantNumeric: "tabular-nums",
            whiteSpace: "nowrap",
            transition: "color 0.3s",
          }}
        >
          {progress.toFixed(1)}%
        </span>
      </div>
      <div
        className="hidden sm:block shrink-0"
        style={{
          width: "80px",
          fontSize: "10.5px",
          color: task.speedColor,
          textAlign: "center",
          fontVariantNumeric: "tabular-nums",
        }}
      >
        {task.speed}
      </div>
      <div
        className="hidden sm:block shrink-0"
        style={{
          width: "56px",
          fontSize: "10.5px",
          color: task.statusColor,
          textAlign: "right",
        }}
      >
        {statusText}
      </div>
    </div>
  );
}

function StatItem({ value, label }: { value: string; label: string }) {
  return (
    <div className="text-center">
      <div className="text-2xl font-bold text-dark-text">{value}</div>
      <div className="text-[10px] text-dark-text-muted mt-0.5">{label}</div>
    </div>
  );
}
