// RFC 3492 (Punycode) parameters for IDNA.
const BASE = 36;
const T_MIN = 1;
const T_MAX = 26;
const SKEW = 38;
const DAMP = 700;
const INITIAL_BIAS = 72;
const INITIAL_N = 128;

const adapt = (delta: number, points: number, first: boolean) => {
  let d = first ? Math.floor(delta / DAMP) : Math.floor(delta / 2);
  d += Math.floor(d / points);
  let k = 0;
  while (d > ((BASE - T_MIN) * T_MAX) / 2) {
    d = Math.floor(d / (BASE - T_MIN));
    k += BASE;
  }
  return k + Math.floor(((BASE - T_MIN + 1) * d) / (d + SKEW));
};

/** a-z are 0-25 and 0-9 are 26-35; anything else is not a digit. */
const digit = (code: number) =>
  code >= 0x61 && code <= 0x7a ? code - 0x61 : code >= 0x30 && code <= 0x39 ? code - 0x16 : BASE;

/** The Unicode text of a Punycode label (without `xn--`), or null if it is not one. */
const decodeLabel = (label: string): string | null => {
  const delimiter = label.lastIndexOf('-');
  const output = Array.from(label.slice(0, Math.max(delimiter, 0)), (c) => c.charCodeAt(0));
  let n = INITIAL_N;
  let bias = INITIAL_BIAS;
  let i = 0;
  for (let index = delimiter > 0 ? delimiter + 1 : 0; index < label.length; ) {
    const before = i;
    for (let weight = 1, k = BASE; ; k += BASE) {
      if (index >= label.length) return null;
      const d = digit(label.charCodeAt(index++));
      if (d >= BASE) return null;
      i += d * weight;
      const t = k <= bias ? T_MIN : k >= bias + T_MAX ? T_MAX : k - bias;
      if (d < t) break;
      weight *= BASE - t;
      if (i > 0x10ffff * BASE) return null;
    }
    const length = output.length + 1;
    bias = adapt(i - before, length, before === 0);
    n += Math.floor(i / length);
    i %= length;
    if (n > 0x10ffff) return null;
    output.splice(i++, 0, n);
  }
  return String.fromCodePoint(...output);
};

/**
 * A host name for reading: each `xn--` label in Unicode, so the backend's
 * `xn--bcher-kva.example` reads `bücher.example`. Display only: entries are
 * stored and sent in the ASCII form, which is what the matcher compares.
 */
export const unicodeHost = (host: string) =>
  host
    .split('.')
    .map((label) => {
      if (!label.startsWith('xn--')) return label;
      const text = decodeLabel(label.slice(4));
      // A label that decodes to ASCII only is not an IDNA label.
      return text && Array.from(text).some((c) => c.codePointAt(0)! > 0x7f) ? text : label;
    })
    .join('.');

/**
 * A host and port as a URL writes them. The backend sends an IPv6 address
 * without brackets, so `2001:db8::1` and port 443 read `[2001:db8::1]:443`.
 */
export const hostWithPort = (host: string, port: number | null | undefined) => {
  const shown = host.includes(':') ? `[${host}]` : host;
  return port ? `${shown}:${port}` : shown;
};

const LOOPBACK_HOSTS = new Set(['localhost', '127.0.0.1', '::1']);

/** This computer, as the backend sends it: `localhost`, `127.0.0.1` or `::1`. */
export const isLoopbackHost = (host: string) => LOOPBACK_HOSTS.has(host);

/**
 * What a request reaches, for reading. A port of this computer reads
 * “port 5432 on this computer”, since `localhost` alone does not say whose
 * it is or which service; any other host reads in Unicode, with its port.
 */
export const requestTarget = (host: string, port: number | null | undefined) =>
  port && isLoopbackHost(host)
    ? `port ${port} on this computer`
    : hostWithPort(unicodeHost(host), port);
