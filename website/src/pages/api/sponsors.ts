import type { APIRoute } from "astro";
import { createHash } from "crypto";

// ── Constants ──────────────────────────────────────────────

const AFDIAN_SLUG = "zerx-lab";
const AFDIAN_PROFILE_API = "https://afdian.com/api/user/get-profile-by-slug";
const AFDIAN_PLANS_API = "https://afdian.com/api/creator/get-plans";
const AFDIAN_SPONSOR_API = "https://afdian.com/api/open/query-sponsor";

// ── Public response types ──────────────────────────────────

export interface AfdianProfile {
  userId: string;
  name: string;
  avatar: string;
  doing: string;
  detail: string;
  category: string;
}

export interface AfdianPlan {
  planId: string;
  name: string;
  price: string;
  desc: string;
  payMonth: number;
  sponsorCount: number;
  independent: boolean;
  permanent: boolean;
}

export interface SponsorItem {
  name: string;
  avatar: string;
  amount: string;
  plan: string;
  firstTime: number;
  lastTime: number;
}

export interface SponsorsPayload {
  profile: AfdianProfile | null;
  plans: AfdianPlan[];
  sponsors: SponsorItem[];
  totalSponsors: number;
  updatedAt: number;
}

// ── In-memory cache ────────────────────────────────────────

let cache: { data: SponsorsPayload; ts: number } | null = null;
const CACHE_TTL = 10 * 60 * 1000; // 10 minutes

// ── Fetch profile (public, no auth) ────────────────────────

async function fetchProfile(): Promise<AfdianProfile | null> {
  try {
    const res = await fetch(`${AFDIAN_PROFILE_API}?url_slug=${AFDIAN_SLUG}`);
    if (!res.ok) return null;

    const json = await res.json();
    if (json.ec !== 200 || !json.data?.user) return null;

    const user = json.data.user;
    const creator = user.creator ?? {};

    return {
      userId: user.user_id,
      name: user.name,
      avatar: user.avatar ?? "",
      doing: creator.doing ?? "",
      detail: creator.detail ?? "",
      category: creator.category?.name ?? "",
    };
  } catch (err) {
    console.error("[sponsors] Failed to fetch profile:", err);
    return null;
  }
}

// ── Fetch plans (public, no auth) ──────────────────────────

async function fetchPlans(userId: string): Promise<AfdianPlan[]> {
  try {
    const res = await fetch(`${AFDIAN_PLANS_API}?user_id=${userId}`);
    if (!res.ok) return [];

    const json = await res.json();
    if (json.ec !== 200 || !json.data?.list) return [];

    const countMap: Record<string, number> = {};
    if (json.data.planIdCountMap) {
      for (const [k, v] of Object.entries(json.data.planIdCountMap)) {
        if (k !== "all_sponsor_can_read") {
          countMap[k] =
            typeof v === "string" ? parseInt(v, 10) || 0 : (v as number);
        }
      }
    }

    return json.data.list
      .filter((p: any) => p.status === 1)
      .sort((a: any, b: any) => parseFloat(a.price) - parseFloat(b.price))
      .map((p: any) => ({
        planId: p.plan_id,
        name: p.name,
        price: p.show_price || p.price,
        desc: p.desc ?? "",
        payMonth: p.pay_month ?? 1,
        sponsorCount: countMap[p.plan_id] ?? 0,
        independent: !!p.independent,
        permanent: !!p.permanent,
      }));
  } catch (err) {
    console.error("[sponsors] Failed to fetch plans:", err);
    return [];
  }
}

// ── Fetch sponsors via Open API (requires auth) ────────────

function signRequest(
  token: string,
  params: string,
  ts: number,
  userId: string,
): string {
  const raw = `${token}params${params}ts${ts}user_id${userId}`;
  return createHash("md5").update(raw).digest("hex");
}

async function fetchSponsors(
  userId: string,
  token: string,
): Promise<{ sponsors: SponsorItem[]; total: number }> {
  const allSponsors: SponsorItem[] = [];
  let page = 1;
  let totalCount = 0;
  const perPage = 50;

  while (page <= 20) {
    const ts = Math.floor(Date.now() / 1000);
    const params = JSON.stringify({ page, per_page: perPage });
    const sig = signRequest(token, params, ts, userId);

    try {
      const res = await fetch(AFDIAN_SPONSOR_API, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ user_id: userId, params, ts, sign: sig }),
      });

      if (!res.ok) break;
      const json = await res.json();
      if (json.ec !== 200) break;

      totalCount = json.data.total_count ?? 0;

      for (const s of json.data.list ?? []) {
        allSponsors.push({
          name: s.user?.name ?? "Anonymous",
          avatar: s.user?.avatar ?? "",
          amount: s.all_sum_amount ?? "0",
          plan: s.current_plan?.name ?? "",
          firstTime: s.first_pay_time ?? 0,
          lastTime: s.last_pay_time ?? 0,
        });
      }

      if (page >= (json.data.total_page ?? 1)) break;
      page++;
    } catch {
      break;
    }
  }

  allSponsors.sort((a, b) => {
    const diff = parseFloat(b.amount) - parseFloat(a.amount);
    return diff !== 0 ? diff : a.firstTime - b.firstTime;
  });

  return { sponsors: allSponsors, total: totalCount };
}

// ── Build full payload ─────────────────────────────────────

async function buildPayload(): Promise<SponsorsPayload> {
  // 1. Fetch profile (public)
  const profile = await fetchProfile();

  // 2. Fetch plans (public, needs userId from profile)
  const plans = profile ? await fetchPlans(profile.userId) : [];

  // 3. Fetch sponsors (needs Open API credentials)
  const afdianUserId = import.meta.env.AFDIAN_USER_ID;
  const afdianToken = import.meta.env.AFDIAN_TOKEN;

  let sponsors: SponsorItem[] = [];
  let totalSponsors = 0;

  if (afdianUserId && afdianToken) {
    const result = await fetchSponsors(afdianUserId, afdianToken);
    sponsors = result.sponsors;
    totalSponsors = result.total;
  }

  return {
    profile,
    plans,
    sponsors,
    totalSponsors,
    updatedAt: Date.now(),
  };
}

// ── API Route ──────────────────────────────────────────────

const CORS_HEADERS = {
  "Content-Type": "application/json",
  "Cache-Control": "public, max-age=300, s-maxage=600",
};

export const GET: APIRoute = async () => {
  // Return cached data if still fresh
  if (cache && Date.now() - cache.ts < CACHE_TTL) {
    return new Response(JSON.stringify(cache.data), {
      status: 200,
      headers: CORS_HEADERS,
    });
  }

  try {
    const payload = await buildPayload();
    cache = { data: payload, ts: Date.now() };

    return new Response(JSON.stringify(payload), {
      status: 200,
      headers: CORS_HEADERS,
    });
  } catch (err) {
    console.error("[sponsors] Unexpected error:", err);

    // Return stale cache if available
    if (cache) {
      return new Response(JSON.stringify(cache.data), {
        status: 200,
        headers: CORS_HEADERS,
      });
    }

    // Absolute fallback
    const fallback: SponsorsPayload = {
      profile: null,
      plans: [],
      sponsors: [],
      totalSponsors: 0,
      updatedAt: Date.now(),
    };

    return new Response(JSON.stringify(fallback), {
      status: 200,
      headers: CORS_HEADERS,
    });
  }
};
