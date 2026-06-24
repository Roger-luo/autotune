// GitHub repo metadata for the docs header's "Star" button.
//
// The star count is fetched once at build time and baked into the static HTML —
// the site is framework-free (the only client JS is the theme toggle), so we
// avoid a client-side API call and the unauthenticated browser rate limit. The
// count therefore refreshes whenever the docs are rebuilt/deployed. Any fetch
// failure (offline, API rate limit, network blip) degrades gracefully to `null`
// and the button renders without a count — the build never fails over this.

export const GITHUB_REPO = 'Roger-luo/autotune';
export const GITHUB_URL = `https://github.com/${GITHUB_REPO}`;

// Memoized so a build that renders N pages makes a single API call, not N.
let cached: Promise<number | null> | undefined;

export function getStarCount(): Promise<number | null> {
  return (cached ??= fetchStarCount());
}

async function fetchStarCount(): Promise<number | null> {
  try {
    const headers: Record<string, string> = {
      Accept: 'application/vnd.github+json',
      'User-Agent': 'autotune-docs',
    };
    // Optional: authenticate in CI to dodge the 60/hr unauthenticated limit
    // shared across GitHub-hosted runner IPs. Read-only; see docs.yml.
    const token = process.env.GITHUB_TOKEN;
    if (token) headers.Authorization = `Bearer ${token}`;

    const res = await fetch(`https://api.github.com/repos/${GITHUB_REPO}`, { headers });
    if (!res.ok) return null;
    const data = (await res.json()) as { stargazers_count?: number };
    return typeof data.stargazers_count === 'number' ? data.stargazers_count : null;
  } catch {
    return null;
  }
}

/** Compact star display: 999 -> "999", 1234 -> "1.2k", 12345 -> "12k". */
export function formatStars(n: number): string {
  if (n < 1000) return String(n);
  const k = n / 1000;
  return `${k.toFixed(k < 10 ? 1 : 0)}k`.replace('.0k', 'k');
}
