/**
 * WebGL renderer probing for the integrated terminal.
 *
 * xterm's WebglAddon is the fastest renderer only when the browser gives it a
 * hardware-accelerated WebGL2 context. On Linux WebKitGTK (especially under
 * Flatpak or with NVIDIA drivers) WebGL frequently falls back to a software
 * rasterizer (llvmpipe/SwiftShader), where the WebGL renderer is *slower*
 * than xterm's built-in DOM renderer. The addon's own fallback only fires on
 * context loss, not on a software context, so we probe up front and skip the
 * addon when the context would be software-rendered.
 */

// Substrings (lowercased) that identify software rasterizers in the
// UNMASKED_RENDERER_WEBGL / RENDERER string. Sources: Chromium's SwiftShader,
// Mesa's llvmpipe/softpipe, ANGLE's software backend, and Windows' fallback
// "Microsoft Basic Render Driver".
const SOFTWARE_RENDERER_PATTERNS = [
  'swiftshader',
  'llvmpipe',
  'softpipe',
  'software rasterizer',
  'microsoft basic render',
  // Truncated on purpose: matches 'ANGLE (Software Adapter ...)' and any
  // future 'ANGLE (Software ...' variant without over-matching hardware ANGLE
  // strings like 'ANGLE (NVIDIA, Direct3D11 ...)'.
  'angle (software',
];

/** True when a WebGL renderer string names a known software rasterizer. */
export function isSoftwareRenderer(renderer: string): boolean {
  const normalized = renderer.toLowerCase();
  return SOFTWARE_RENDERER_PATTERNS.some((pattern) => normalized.includes(pattern));
}

// Renderer strings that are privacy masks rather than real device names, so
// they carry no hardware-vs-software signal. WebKit reports the literal
// "Apple GPU" from Safari 14+ as anti-fingerprinting — on macOS that is real
// Apple-silicon hardware, but Linux WebKitGTK returns the same mask for any
// backend, including llvmpipe. Verified on WebKitGTK 2.x where the unmasked
// renderer probe returns "Apple GPU" while the process renders on Intel Xe.
const MASKED_RENDERER_PATTERNS = ['apple gpu'];

/**
 * True when a WebGL renderer string is a known privacy mask that hides the
 * real device, so no software/hardware classification is possible.
 */
export function isMaskedRenderer(renderer: string): boolean {
  const normalized = renderer.toLowerCase();
  return MASKED_RENDERER_PATTERNS.some((pattern) => normalized.includes(pattern));
}

export type WebglProbeResult =
  // WebGL2 context exists and the renderer string looks hardware-backed.
  | 'hardware'
  // WebGL2 context exists but is a known software rasterizer.
  | 'software'
  // No WebGL2 context at all (addon would throw anyway).
  | 'unavailable'
  // Context exists but the renderer string is a known privacy mask (e.g.
  // "Apple GPU" on WebKit) that hides the real backend. No signal either way.
  | 'generic'
  // Context exists but the renderer string could not be read — treat as
  // usable, matching the previous always-try behavior.
  | 'unknown';

/**
 * Probe the WebGL2 renderer with a throwaway canvas. WebglAddon requires
 * WebGL2 specifically, so only `webgl2` is probed. The probe context is
 * released via WEBGL_lose_context so it doesn't count against the browser's
 * context limit.
 */
export function probeWebglRenderer(
  createCanvas: () => HTMLCanvasElement = () => document.createElement('canvas')
): WebglProbeResult {
  let gl: WebGL2RenderingContext | null = null;
  try {
    gl = createCanvas().getContext('webgl2');
  } catch {
    return 'unavailable';
  }
  if (!gl) return 'unavailable';
  try {
    const debugInfo = gl.getExtension('WEBGL_debug_renderer_info') as {
      UNMASKED_RENDERER_WEBGL: number;
    } | null;
    const renderer = debugInfo
      ? (gl.getParameter(debugInfo.UNMASKED_RENDERER_WEBGL) as string | null)
      : (gl.getParameter(gl.RENDERER) as string | null);
    if (typeof renderer !== 'string' || renderer.length === 0) return 'unknown';
    if (isMaskedRenderer(renderer)) return 'generic';
    return isSoftwareRenderer(renderer) ? 'software' : 'hardware';
  } catch {
    return 'unknown';
  } finally {
    try {
      gl.getExtension('WEBGL_lose_context')?.loseContext();
    } catch {
      /* best-effort cleanup */
    }
  }
}

// The renderer doesn't change within a session, so probe once and cache. Each
// terminal mount (multiple kept-alive workspaces) reuses the same answer.
let cachedDecision: boolean | null = null;

/**
 * Whether the terminal should load xterm's WebglAddon. True for hardware,
 * masked ('generic'), or unreadable ('unknown') WebGL2 contexts — masked and
 * unreadable strings carry no signal, so we keep the previous always-try
 * behavior rather than guessing software. False when WebGL2 is missing or
 * provably software-rendered, where xterm's DOM renderer is the faster choice.
 */
export function shouldUseWebglRenderer(): boolean {
  if (cachedDecision === null) {
    const result = probeWebglRenderer();
    cachedDecision = result === 'hardware' || result === 'generic' || result === 'unknown';
  }
  return cachedDecision;
}

/** Test-only: clear the memoized probe decision. */
export function resetWebglProbeCacheForTests(): void {
  cachedDecision = null;
}
