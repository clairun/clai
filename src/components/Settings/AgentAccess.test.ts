import { describe, expect, it } from 'vitest';
import type { LocalPortGrant, NetworkPolicy, SandboxStatus } from '../../generated/bindings';
import {
  accessSummary, localPortOf, localPortsDeniedBy, parseAllowEntry, parseHostEntry, type AccessDraft,
} from './AgentAccess';
import hostEntryCases from './hostEntryCases.json';

const DRAFT: AccessDraft = {
  grants: [],
  shellMode: 'restricted',
  askFirst: [],
  web: true,
  sessionBusMode: 'keyring_only',
  networkAccess: null,
};

const LINUX: SandboxStatus = {
  available: true, backend: 'bubblewrap', allowlistAvailable: true, allowlistUnavailableReason: null,
  socketIsolationAvailable: true, socketIsolationUnavailableReason: null,
  sessionBusFilterAvailable: true, sessionBusFilterUnavailableReason: null,
};

const summary = (draft: Partial<AccessDraft>, sandbox: SandboxStatus | null = LINUX) =>
  accessSummary({ ...DRAFT, ...draft }, sandbox);
const bus = (draft: Partial<AccessDraft>, sandbox: SandboxStatus | null = LINUX) =>
  summary(draft, sandbox).find((line) => line.startsWith('Bus'));
const network = (networkAccess: NetworkPolicy) =>
  summary({ networkAccess }).find((line) => line.startsWith('Network'));

const SSH: LocalPortGrant = { kind: 'program', port: 5432, program: '/usr/bin/ssh' };
const POSTGRES: LocalPortGrant = { kind: 'user', port: 5433, uid: 120, user: 'postgres' };
const TWELVE_HOSTS = Array.from({ length: 12 }, (_, i) => `host${i}.example`);

describe('accessSummary', () => {
  it('names the bus an agent gets', () => {
    expect(bus({ sessionBusMode: 'deny' })).toBe('Bus off');
    expect(bus({ sessionBusMode: 'keyring_only' })).toBe('Bus: keyring only');
    expect(bus({ sessionBusMode: 'full' })).toBe('Bus: full');
  });

  it('says keyring only is no bus where the bus cannot be filtered', () => {
    const unfiltered = { ...LINUX, sessionBusFilterAvailable: false, sessionBusFilterUnavailableReason: 'x' };
    expect(bus({ sessionBusMode: 'keyring_only' }, unfiltered)).toBe('Bus off (keyring only unavailable here)');
    expect(bus({ sessionBusMode: 'full' }, unfiltered)).toBe('Bus: full');
    expect(bus({ sessionBusMode: 'deny' }, unfiltered)).toBe('Bus off');
  });

  it('counts the local ports of an Allowlist, when it has any', () => {
    const allowlist: NetworkPolicy = { mode: 'allowlist', allowDomains: TWELVE_HOSTS, denyDomains: [] };
    expect(network({ ...allowlist, localPorts: [SSH] })).toBe('Network: Allowlist (12 hosts, 1 local port)');
    expect(network({ ...allowlist, localPorts: [SSH, POSTGRES] })).toBe('Network: Allowlist (12 hosts, 2 local ports)');
    expect(network({ ...allowlist, localPorts: [] })).toBe('Network: Allowlist (12 hosts)');
    expect(network(allowlist)).toBe('Network: Allowlist (12 hosts)');
    expect(network({ ...allowlist, mode: 'off', localPorts: [SSH] })).toBe('Network: Off');
  });
});

const THIS_COMPUTER = 'This computer is allowed one port at a time, as in localhost:5432, under Local ports.';
const LOCAL_PORT = 'Local ports go in the Local ports list, which checks the program listening.';
const DENY_LOCAL_PORT = 'To refuse a local port, remove it from Local ports; deny localhost to refuse them all.';
const BRACKETS = 'Put the IPv6 address in brackets, as in [::1]:5432, under Local ports.';

// The cases `local_ports_are_parsed_by_one_rule` and `entries_are_parsed_by_one_rule`
// in network/matcher.rs run.
describe('host entries and local ports', () => {
  it('reads a port of this computer only where the backend reads one', () => {
    for (const { entry, port } of hostEntryCases.localPorts.accepted) {
      expect(localPortOf(entry), entry).toBe(port);
    }
    for (const entry of [...hostEntryCases.localPorts.rejected, ...hostEntryCases.localPorts.notLocal]) {
      expect(localPortOf(entry), entry).toBeNull();
    }
  });

  it('sends a port of this computer to the local ports, which can only allow it', () => {
    for (const { entry } of hostEntryCases.localPorts.accepted) {
      expect(parseAllowEntry(entry), entry).toEqual({ error: LOCAL_PORT });
      expect(parseHostEntry(entry), entry).toEqual({ error: DENY_LOCAL_PORT });
    }
  });

  it('takes nothing the backend reads as this computer as an allowed host', () => {
    for (const entry of hostEntryCases.localPorts.rejected) {
      expect(parseAllowEntry(entry), entry).toHaveProperty('error');
    }
    expect(parseAllowEntry('::1:5432')).toEqual({ error: BRACKETS });
    expect(parseAllowEntry('64:ff9b::7f00:1:80')).toEqual({ error: BRACKETS });
    expect(parseAllowEntry('2001:db8::1:80')).toBe('2001:db8::1:80');
  });

  it('refuses this computer on the allowed hosts and keeps it for the denied hosts', () => {
    for (const entry of hostEntryCases.allowOnlyRejected) {
      expect(parseHostEntry(entry), entry).toEqual(expect.any(String));
      expect(parseAllowEntry(entry), entry).toEqual({ error: THIS_COMPUTER });
    }
    for (const { entry, matched } of hostEntryCases.accepted) {
      expect(parseAllowEntry(entry), entry).toBe(matched);
    }
    expect(parseAllowEntry('::ffff:10.0.0.1')).toBe('::ffff:a00:1');
    expect(parseAllowEntry('::7f00:1')).toBe('::7f00:1');
  });

  it('finds the denied entry that refuses every local port', () => {
    expect(localPortsDeniedBy(['bad.example', 'localhost'])).toBe('localhost');
    expect(localPortsDeniedBy(['127.0.0.1'])).toBe('127.0.0.1');
    expect(localPortsDeniedBy(['::1'])).toBe('::1');
    expect(localPortsDeniedBy(['LocalHost.'])).toBe('LocalHost.');
    expect(localPortsDeniedBy(['*.localhost', 'app.localhost', '127.0.0.2', 'localhost:5432'])).toBeUndefined();
    expect(localPortsDeniedBy([])).toBeUndefined();
  });
});
