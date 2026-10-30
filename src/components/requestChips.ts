/** A spoken (Fleet) chip carries the short label, its tooltip the full one. */
export const chipText = (spoken: boolean, full: string, short: string, why?: string) =>
  spoken ? { title: why ? `${full}. ${why}` : full, children: short } : { title: why, children: full };
