/**
 * HTML preview bundler
 *
 * The artifact preview renders HTML into a unique-origin `srcDoc` iframe
 * (sandboxed, no `allow-same-origin`). A `srcDoc` document has base URL
 * `about:srcdoc`, so any *relative* reference a report makes — an external
 * stylesheet, a script, an image, a font behind a CSS `url(...)` — resolves
 * to nothing and the report renders unstyled.
 *
 * Rather than fight the sandbox with a `<base href>` + custom protocol
 * (fragile on WebKitGTK/Flatpak), this module turns a multi-file report into
 * a single self-contained HTML string: it walks the parsed document, pulls
 * every local sibling through `readWorkspaceFileBase64`, and inlines it
 * (CSS/JS as text, binaries as `data:` URIs). The result drops straight into
 * the existing `srcDoc` with the security model unchanged.
 *
 * Absolute and remote references (`http(s):`, protocol-relative `//`,
 * `data:`, `blob:`, root-absolute `/…`, in-page `#…`) are left untouched.
 * Anything that fails to load is left as-is so a single missing asset never
 * breaks the rest of the preview.
 */

import { readWorkspaceFileBase64 } from '../workspace/client';

// ── Path helpers ───────────────────────────────────────────────────────────

const dirname = (path: string): string => {
  const slash = path.lastIndexOf('/');
  return slash === -1 ? '' : path.slice(0, slash);
};

/**
 * Join a relative reference onto a base directory and collapse `.`/`..`
 * segments, yielding a clean workspace-relative path. The Rust side
 * re-normalizes and rejects anything escaping the workspace root, so this is
 * about producing a sensible lookup key, not a security boundary.
 */
const joinPath = (baseDir: string, rel: string): string => {
  const parts = (baseDir ? baseDir.split('/') : []).concat(rel.split('/'));
  const out: string[] = [];
  for (const part of parts) {
    if (part === '' || part === '.') continue;
    if (part === '..') {
      out.pop();
      continue;
    }
    out.push(part);
  }
  return out.join('/');
};

/** Strip a `?query` / `#fragment` suffix used for cache-busting fonts etc. */
const stripQuery = (url: string): string => {
  const cut = url.search(/[?#]/);
  return cut === -1 ? url : url.slice(0, cut);
};

/**
 * Whether a URL points at a local sibling we can inline. Rejects empty
 * values, in-page anchors, root-absolute paths, and anything carrying a URL
 * scheme (`http:`, `https:`, `data:`, `blob:`, `mailto:`, protocol-relative
 * `//…`, …).
 */
const isInlinableUrl = (url: string | null | undefined): url is string => {
  if (!url) return false;
  const u = url.trim();
  if (u === '' || u.startsWith('#') || u.startsWith('/')) return false;
  // Any leading `scheme:` (or protocol-relative `//`, caught above) is remote.
  if (/^[a-z][a-z0-9+.-]*:/i.test(u)) return false;
  return true;
};

/**
 * Whether a link href points at another file in the workspace (a relative or
 * root-absolute path) rather than somewhere external. External = empty, an
 * in-page `#anchor`, protocol-relative `//host`, or any explicit URL scheme
 * (`https:`, `mailto:`, …). Used to decide whether a click in a preview
 * should navigate to a sibling artifact or open in the OS browser.
 */
export function isWorkspaceRelativeHref(href: string | null | undefined): boolean {
  const h = (href ?? '').trim();
  if (!h || h.startsWith('#') || h.startsWith('//')) return false;
  if (/^[a-z][a-z0-9+.-]*:/i.test(h)) return false; // has a scheme → external
  return true;
}

/**
 * Resolve a relative href that appears *inside* the artifact `baseFilePath`
 * into a clean workspace-relative path, dropping any `?query`/`#fragment`.
 * Used to turn an in-document link (`<a href="2026-05-31.html">`) into the
 * sibling artifact's workspace path so the preview can navigate to it.
 * A leading `/` is treated as workspace-root-relative.
 */
export function resolveWorkspacePath(baseFilePath: string, href: string): string {
  const cleaned = stripQuery((href ?? '').trim());
  if (!cleaned) return '';
  const baseDir = cleaned.startsWith('/') ? '' : dirname(baseFilePath);
  return joinPath(baseDir, cleaned);
}

// ── Resource reader (deduplicated per bundle) ────────────────────────────────

interface ResourceReader {
  readText: (path: string) => Promise<string>;
  readDataUri: (path: string) => Promise<string>;
}

/** Decode a base64 payload from `readWorkspaceFileBase64` as UTF-8 text. */
export const base64ToText = (base64: string): string => {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return new TextDecoder('utf-8').decode(bytes);
};

const makeReader = (workspaceId: string): ResourceReader => {
  // One read per distinct path, even when several elements reference it.
  const cache = new Map<string, ReturnType<typeof readWorkspaceFileBase64>>();
  const read = (path: string) => {
    let pending = cache.get(path);
    if (!pending) {
      pending = readWorkspaceFileBase64(workspaceId, path);
      cache.set(path, pending);
    }
    return pending;
  };
  return {
    readText: async (path) => base64ToText((await read(path)).base64),
    readDataUri: async (path) => {
      const { mime, base64 } = await read(path);
      return `data:${mime};base64,${base64}`;
    },
  };
};

// ── Async string replacement ─────────────────────────────────────────────────

const replaceAsync = async (
  input: string,
  regex: RegExp,
  replacer: (match: RegExpMatchArray) => Promise<string>,
): Promise<string> => {
  const matches = [...input.matchAll(regex)];
  if (matches.length === 0) return input;
  const replacements = await Promise.all(matches.map((m) => replacer(m)));
  let result = '';
  let last = 0;
  matches.forEach((match, i) => {
    const index = match.index ?? 0;
    result += input.slice(last, index) + replacements[i];
    last = index + match[0].length;
  });
  return result + input.slice(last);
};

// ── CSS inlining (handles @import and url(), recursively) ────────────────────

// `@import "x";` or `@import url("x") screen;` — capture the URL from either form.
const CSS_IMPORT_RE = /@import\s+(?:url\(\s*(['"]?)([^'")]+)\1\s*\)|(['"])([^'"]+)\3)\s*[^;]*;/gi;
// `url(x)`, `url('x')`, `url("x")` — base64 data URIs contain no parens, so the
// negated class is safe even after earlier substitutions.
const CSS_URL_RE = /url\(\s*(['"]?)([^'")]+)\1\s*\)/gi;

const MAX_CSS_DEPTH = 8;

const inlineCss = async (
  reader: ResourceReader,
  cssText: string,
  cssDir: string,
  depth: number,
): Promise<string> => {
  if (depth > MAX_CSS_DEPTH) return cssText;

  // Resolve @import first so the pulled-in CSS also gets its url()s inlined,
  // relative to the imported file's own directory.
  let out = await replaceAsync(cssText, CSS_IMPORT_RE, async (match) => {
    const url = match[2] ?? match[4];
    if (!isInlinableUrl(url)) return match[0];
    const path = joinPath(cssDir, stripQuery(url));
    try {
      const imported = await reader.readText(path);
      return inlineCss(reader, imported, dirname(path), depth + 1);
    } catch {
      return match[0];
    }
  });

  out = await replaceAsync(out, CSS_URL_RE, async (match) => {
    const url = match[2];
    if (!isInlinableUrl(url)) return match[0];
    const path = joinPath(cssDir, stripQuery(url));
    try {
      return `url("${await reader.readDataUri(path)}")`;
    } catch {
      return match[0];
    }
  });

  return out;
};

// ── srcset rewriting ─────────────────────────────────────────────────────────

const rewriteSrcset = async (
  reader: ResourceReader,
  srcset: string,
  baseDir: string,
): Promise<string> => {
  const rewritten = await Promise.all(
    srcset
      .split(',')
      .map((c) => c.trim())
      .filter(Boolean)
      .map(async (candidate) => {
        const [url, ...descriptor] = candidate.split(/\s+/);
        if (!isInlinableUrl(url)) return candidate;
        try {
          const dataUri = await reader.readDataUri(joinPath(baseDir, stripQuery(url)));
          return descriptor.length ? `${dataUri} ${descriptor.join(' ')}` : dataUri;
        } catch {
          return candidate;
        }
      }),
  );
  return rewritten.join(', ');
};

// ── Main entry point ─────────────────────────────────────────────────────────

/**
 * Inline every local resource an HTML artifact references and return a
 * single self-contained HTML string suitable for an isolated `srcDoc`
 * iframe. `htmlPath` is the artifact's workspace-relative path (e.g.
 * `reports/2026-05-31.html`); sibling resources resolve against its
 * directory. Never throws — on any failure the original markup is returned.
 */
export async function bundleHtmlForPreview(
  workspaceId: string,
  htmlPath: string,
  rawHtml: string,
): Promise<string> {
  if (typeof rawHtml !== 'string' || rawHtml.length === 0) return rawHtml;

  try {
    const reader = makeReader(workspaceId);
    const baseDir = dirname(htmlPath);
    const doc = new DOMParser().parseFromString(rawHtml, 'text/html');

    // Stylesheets: <link rel="stylesheet" href> → inlined <style>.
    const stylesheetLinks = [
      ...doc.querySelectorAll<HTMLLinkElement>('link[rel~="stylesheet"][href]'),
    ];
    await Promise.all(
      stylesheetLinks.map(async (link) => {
        const href = link.getAttribute('href');
        if (!isInlinableUrl(href)) return;
        const path = joinPath(baseDir, stripQuery(href!));
        try {
          const css = await inlineCss(reader, await reader.readText(path), dirname(path), 0);
          const style = doc.createElement('style');
          // Preserve a media constraint if the original <link> carried one.
          const media = link.getAttribute('media');
          if (media) style.setAttribute('media', media);
          style.textContent = css;
          link.replaceWith(style);
        } catch {
          /* leave the <link> untouched */
        }
      }),
    );

    // Inline <style> blocks: resolve their url()/@import against the HTML dir.
    const styleBlocks = [...doc.querySelectorAll<HTMLStyleElement>('style')];
    await Promise.all(
      styleBlocks.map(async (style) => {
        if (!style.textContent) return;
        try {
          style.textContent = await inlineCss(reader, style.textContent, baseDir, 0);
        } catch {
          /* keep original */
        }
      }),
    );

    // Scripts: <script src> → inlined <script> (type/defer preserved).
    const scripts = [...doc.querySelectorAll<HTMLScriptElement>('script[src]')];
    await Promise.all(
      scripts.map(async (script) => {
        const src = script.getAttribute('src');
        if (!isInlinableUrl(src)) return;
        const path = joinPath(baseDir, stripQuery(src!));
        try {
          const js = await reader.readText(path);
          const inline = doc.createElement('script');
          const type = script.getAttribute('type');
          if (type) inline.setAttribute('type', type);
          inline.textContent = js;
          script.replaceWith(inline);
        } catch {
          /* leave the <script src> untouched */
        }
      }),
    );

    // Media `src`/`poster` attributes → data URIs.
    const mediaEls = [
      ...doc.querySelectorAll<HTMLElement>(
        'img[src], source[src], video[src], audio[src], video[poster], input[type="image"][src]',
      ),
    ];
    await Promise.all(
      mediaEls.map(async (el) => {
        for (const attr of ['src', 'poster']) {
          const value = el.getAttribute(attr);
          if (!isInlinableUrl(value)) continue;
          try {
            el.setAttribute(attr, await reader.readDataUri(joinPath(baseDir, stripQuery(value!))));
          } catch {
            /* keep original */
          }
        }
      }),
    );

    // `srcset` (responsive images and <picture><source>).
    const srcsetEls = [...doc.querySelectorAll<HTMLElement>('[srcset]')];
    await Promise.all(
      srcsetEls.map(async (el) => {
        const srcset = el.getAttribute('srcset');
        if (!srcset) return;
        try {
          el.setAttribute('srcset', await rewriteSrcset(reader, srcset, baseDir));
        } catch {
          /* keep original */
        }
      }),
    );

    // Favicons, apple-touch-icons, and preload hints → data URIs.
    const assetLinks = [
      ...doc.querySelectorAll<HTMLLinkElement>(
        'link[rel~="icon"][href], link[rel="apple-touch-icon"][href], link[rel="mask-icon"][href], link[rel="preload"][href]',
      ),
    ];
    await Promise.all(
      assetLinks.map(async (link) => {
        const href = link.getAttribute('href');
        if (!isInlinableUrl(href)) return;
        try {
          link.setAttribute('href', await reader.readDataUri(joinPath(baseDir, stripQuery(href!))));
        } catch {
          /* keep original */
        }
      }),
    );

    // Inline `style="…url(…)…"` attributes (e.g. hero background images).
    const styledEls = [...doc.querySelectorAll<HTMLElement>('[style*="url("]')];
    await Promise.all(
      styledEls.map(async (el) => {
        const inlineStyle = el.getAttribute('style');
        if (!inlineStyle) return;
        try {
          el.setAttribute('style', await inlineCss(reader, inlineStyle, baseDir, 0));
        } catch {
          /* keep original */
        }
      }),
    );

    const doctype = doc.doctype ? `<!DOCTYPE ${doc.doctype.name}>\n` : '<!DOCTYPE html>\n';
    return doctype + doc.documentElement.outerHTML;
  } catch {
    // DOMParser/serialization should never fail on real input, but if it
    // does, a styled-incorrectly preview beats no preview at all.
    return rawHtml;
  }
}
