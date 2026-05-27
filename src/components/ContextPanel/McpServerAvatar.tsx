import React, { useMemo, useState } from 'react';
import type { McpServerResponse } from '../../generated/bindings';
import styles from './McpServerAvatar.module.css';

const getOriginFaviconUrl = (server?: McpServerResponse | null): string | null => {
  const transport = server?.transport;
  if (!transport || transport.type !== 'http' || !transport.url) {
    return null;
  }

  try {
    const url = new URL(transport.url);
    return `${url.origin}/favicon.ico`;
  } catch {
    return null;
  }
};

const getFallbackLabel = (server?: McpServerResponse | null): string => {
  const transport = server?.transport;
  const command = transport && transport.type === 'stdio' ? transport.command : undefined;
  const source = server?.name || command || 'M';
  const parts = source.trim().split(/\s+/).filter(Boolean);
  if (parts.length >= 2) {
    return `${parts[0][0]}${parts[1][0]}`.toUpperCase();
  }
  return source.slice(0, 2).toUpperCase();
};

interface McpServerAvatarProps {
  server?: McpServerResponse | null;
  disabled?: boolean;
}

const McpServerAvatar = ({ server, disabled = false }: McpServerAvatarProps) => {
  const faviconUrl = useMemo(() => getOriginFaviconUrl(server), [server]);
  const fallbackLabel = useMemo(() => getFallbackLabel(server), [server]);
  const [imageFailed, setImageFailed] = useState(false);

  if (faviconUrl && !imageFailed) {
    return (
      <span className={styles.avatar}>
        <img
          src={faviconUrl}
          alt=""
          className={styles.image}
          onError={() => setImageFailed(true)}
        />
      </span>
    );
  }

  return (
    <span className={`${styles.avatar} ${disabled ? styles.fallbackDisabled : ''}`}>
      {fallbackLabel}
    </span>
  );
};

export default McpServerAvatar;
