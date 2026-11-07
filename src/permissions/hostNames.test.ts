import { describe, expect, it } from 'vitest';
import { hostWithPort, requestTarget, unicodeHost } from './hostNames';

describe('unicodeHost', () => {
  it('reads each xn-- label in Unicode', () => {
    expect(unicodeHost('xn--bcher-kva.example')).toBe('bücher.example');
    expect(unicodeHost('*.xn--mnchen-3ya.de')).toBe('*.münchen.de');
    expect(unicodeHost('xn--fiqs8s')).toBe('中国');
    expect(unicodeHost('xn--80ak6aa92e.com')).toBe('аррӏе.com');
    expect(unicodeHost('xn--ls8h.la')).toBe('💩.la');
  });

  it('leaves ASCII names, IP addresses and labels that do not decode as they are', () => {
    for (const host of ['github.com', '*.example.org', '10.0.0.5', '2001:db8::1', 'xn--.example', 'xn--abc-.example', 'xn--a-?.example']) {
      expect(unicodeHost(host)).toBe(host);
    }
  });
});

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

describe('requestTarget', () => {
  it.each([
    ['localhost', 5432, 'port 5432 on this computer'],
    ['127.0.0.1', 5432, 'port 5432 on this computer'],
    ['::1', 5432, 'port 5432 on this computer'],
    ['localhost', null, 'localhost'],
    ['127.0.0.2', 80, '127.0.0.2:80'],
    ['xn--bcher-kva.example', 443, 'bücher.example:443'],
    ['2001:db8::1', 443, '[2001:db8::1]:443'],
  ] as const)('reads %s with port %s as “%s”', (host, port, text) => {
    expect(requestTarget(host, port)).toBe(text);
  });
});
