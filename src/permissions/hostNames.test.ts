import { describe, expect, it } from 'vitest';
import { hostWithPort } from './hostNames';

describe('hostWithPort', () => {
  it('brackets an IPv6 address so its port cannot be read as part of it', () => {
    expect(hostWithPort('2001:db8::1', 443)).toBe('[2001:db8::1]:443');
    expect(hostWithPort('2001:db8::1', null)).toBe('[2001:db8::1]');
  });

  it('writes a name or an IPv4 address as it is', () => {
    expect(hostWithPort('mirror.example.org', 443)).toBe('mirror.example.org:443');
    expect(hostWithPort('10.0.0.5', 8080)).toBe('10.0.0.5:8080');
    expect(hostWithPort('example.org', null)).toBe('example.org');
  });
});
