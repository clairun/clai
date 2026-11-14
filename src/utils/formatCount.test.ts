import { describe, expect, it } from 'vitest';
import { formatCount } from './formatCount';

describe('formatCount', () => {
  it('groups digits by locale', () => {
    expect(formatCount(25583, false, 'en-US')).toBe('25,583');
    expect(formatCount(25583, false, 'es-ES')).toBe('25.583');
    expect(formatCount(1234567, false, 'de-DE')).toBe('1.234.567');
  });

  it('marks a capped count as a lower bound in every locale', () => {
    expect(formatCount(21725, true, 'en-US')).toBe('21,725+');
    expect(formatCount(21725, true, 'es-ES')).toBe('21.725+');
  });

  it('leaves small counts bare', () => {
    expect(formatCount(0, false, 'en-US')).toBe('0');
    expect(formatCount(127, false, 'es-ES')).toBe('127');
  });
});
