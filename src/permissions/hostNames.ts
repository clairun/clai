/**
 * A host and port as a URL writes them. The backend sends an IPv6 address
 * without brackets, so `2001:db8::1` and port 443 read `[2001:db8::1]:443`.
 */
export const hostWithPort = (host: string, port: number | null | undefined) => {
  const shown = host.includes(':') ? `[${host}]` : host;
  return port ? `${shown}:${port}` : shown;
};
