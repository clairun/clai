import { Fragment, type ReactNode } from 'react';

/** A spoken (Fleet) chip carries the short label, its tooltip the full one. */
export const chipText = (spoken: boolean, full: string, short: string, why?: string) =>
  spoken ? { title: why ? `${full}. ${why}` : full, children: short } : { title: why, children: full };

export const quoted = (name: string) => `“${name}”`;

/** "a", "a and b", "a, b and c". */
export const listOf = (items: ReactNode[]) =>
  items.map((item, i) => (
    <Fragment key={i}>
      {i === 0 ? '' : i === items.length - 1 ? ' and ' : ', '}
      {item}
    </Fragment>
  ));
