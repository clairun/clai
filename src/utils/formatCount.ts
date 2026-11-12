/**
 * Locale-aware counts for the UI. `locale` defaults to the runtime's own
 * (the webview follows the system), the same source `toLocaleString()`
 * uses; tests pass one explicitly.
 */
const formatters = new Map<string, Intl.NumberFormat>();

const formatterFor = (locale?: string): Intl.NumberFormat => {
  const key = locale ?? '';
  let formatter = formatters.get(key);
  if (!formatter) {
    formatter = new Intl.NumberFormat(locale, { maximumFractionDigits: 0 });
    formatters.set(key, formatter);
  }
  return formatter;
};

/**
 * `capped` marks a lower bound rather than a measurement (a walk that stopped
 * on its budget): it renders with a trailing "+" so no display presents a
 * saturated count as exact.
 */
export const formatCount = (value: number, capped = false, locale?: string): string => {
  const text = formatterFor(locale).format(value);
  return capped ? `${text}+` : text;
};
