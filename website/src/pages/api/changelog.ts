/**
 * GET /api/changelog?page=1&per_page=10&since=v0.0.2
 *
 * 代理 GitHub Releases API，获取私有仓库的发布记录（分页）。
 * 服务端持有 GITHUB_TOKEN，前端无需暴露凭据。
 *
 * Query params:
 *   page     - 页码，从 1 开始，默认 1
 *   per_page - 每页条数，默认 10，最大 100
 *   since    - 可选，起始版本号（含），如 "v0.0.2" 或 "0.0.2"
 *              代码保留字段，用于控制不展示某些旧版本
 *
 * 返回格式:
 * {
 *   releases: [ { tag, version, published_at, body } ],
 *   page: 1,
 *   per_page: 10,
 *   has_more: true
 * }
 */

import type { APIRoute } from "astro";

export const prerender = false;

const GITHUB_REPO = import.meta.env.GITHUB_REPO || "user/x_down";
const GITHUB_TOKEN = import.meta.env.GITHUB_TOKEN || "";

// ── 全量缓存：拉取 GitHub 所有 release 后缓存，分页在返回时切片 ──
let allCache: { releases: FilteredRelease[]; timestamp: number } | null = null;
const CACHE_TTL = 300_000; // 5 分钟

interface GitHubRelease {
  tag_name: string;
  name: string;
  published_at: string;
  draft: boolean;
  prerelease: boolean;
  body: string;
}

interface FilteredRelease {
  tag: string;
  version: string;
  published_at: string;
  body: string;
}

/** 将 tag 转为可比较的版本数组，如 "v0.0.3" → [0, 0, 3] */
function parseVersion(tag: string): number[] {
  return tag
    .replace(/^v/, "")
    .split(".")
    .map((s) => {
      const n = parseInt(s, 10);
      return isNaN(n) ? 0 : n;
    });
}

/** 比较两个版本：a >= b 返回 true */
function versionGte(a: number[], b: number[]): boolean {
  const len = Math.max(a.length, b.length);
  for (let i = 0; i < len; i++) {
    const va = a[i] ?? 0;
    const vb = b[i] ?? 0;
    if (va > vb) return true;
    if (va < vb) return false;
  }
  return true;
}

/** 解析 GitHub Link header 中的 next URL */
function parseLinkNext(header: string | null): string | null {
  if (!header) return null;
  const match = header.match(/<([^>]+)>;\s*rel="next"/);
  return match ? match[1] : null;
}

/** 拉取 GitHub 全部 releases（自动跟随分页） */
async function fetchAllGitHubReleases(): Promise<GitHubRelease[]> {
  const all: GitHubRelease[] = [];
  let url: string | null =
    `https://api.github.com/repos/${GITHUB_REPO}/releases?per_page=100`;

  while (url) {
    const res = await fetch(url, {
      headers: {
        Authorization: `Bearer ${GITHUB_TOKEN}`,
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
      },
    });

    if (!res.ok) {
      throw new Error(`GitHub API ${res.status}: ${await res.text()}`);
    }

    const page: GitHubRelease[] = await res.json();
    all.push(...page);

    // 跟随 Link: <...>; rel="next"
    url = parseLinkNext(res.headers.get("Link"));
  }

  return all;
}

/** 获取经过过滤和排序的全量 release 列表（带缓存） */
async function getCachedReleases(since: string): Promise<FilteredRelease[]> {
  // 缓存未过期则直接用
  if (!allCache || Date.now() - allCache.timestamp > CACHE_TTL) {
    const raw = await fetchAllGitHubReleases();

    // 过滤草稿和预发布，按时间倒序
    const filtered = raw
      .filter((r) => !r.draft && !r.prerelease)
      .sort(
        (a, b) =>
          new Date(b.published_at).getTime() - new Date(a.published_at).getTime(),
      )
      .map((r) => ({
        tag: r.tag_name,
        version: r.tag_name.replace(/^v/, ""),
        published_at: r.published_at,
        body: r.body || "",
      }));

    allCache = { releases: filtered, timestamp: Date.now() };
  }

  let releases = allCache.releases;

  // since 过滤（版本 >= since）
  if (since) {
    const sinceVer = parseVersion(since);
    releases = releases.filter((r) => versionGte(parseVersion(r.tag), sinceVer));
  }

  return releases;
}

export const GET: APIRoute = async ({ url }) => {
  const sinceParam = url.searchParams.get("since")?.trim() || "";
  const page = Math.max(1, parseInt(url.searchParams.get("page") || "1", 10) || 1);
  const perPage = Math.min(
    100,
    Math.max(1, parseInt(url.searchParams.get("per_page") || "10", 10) || 10),
  );

  if (!GITHUB_TOKEN) {
    return new Response(
      JSON.stringify({ error: "Server misconfigured: missing GITHUB_TOKEN" }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }

  try {
    const all = await getCachedReleases(sinceParam);
    const start = (page - 1) * perPage;
    const sliced = all.slice(start, start + perPage);

    const data = {
      releases: sliced,
      page,
      per_page: perPage,
      has_more: start + perPage < all.length,
    };

    return new Response(JSON.stringify(data), {
      status: 200,
      headers: {
        "Content-Type": "application/json",
        "Cache-Control": "public, s-maxage=300, stale-while-revalidate=600",
      },
    });
  } catch (err) {
    return new Response(
      JSON.stringify({ error: "Failed to fetch releases", detail: String(err) }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }
};
