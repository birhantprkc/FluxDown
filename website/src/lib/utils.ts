import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** FluxDown 开源仓库地址 */
export const GITHUB_REPO_URL = "https://github.com/zerx-lab/FluxDown";

/** Web 版演示站地址（带公开演示令牌，访客点开即自动登录） */
export const DEMO_URL =
  "https://demo.zerx.dev/?token=fxd_bfc6b03e8e494ec8907415a2e8a0b21b";
