import { useState, useEffect } from 'react';

type OsName = 'windows' | 'macos' | 'linux' | 'unknown';

interface PlatformInfo {
  os: OsName;
}

export function usePlatform(): PlatformInfo {
  const [platform, setPlatform] = useState<PlatformInfo>({
    os: 'unknown',
  });

  useEffect(() => {
    const detectPlatform = async () => {
      try {
        // Import the platform function from the Tauri plugin
        const { platform: tauriPlatform } = await import('@tauri-apps/plugin-os');
        const osType = await tauriPlatform();

        const platformMap: Record<string, OsName> = {
          windows: 'windows',
          macos: 'macos',
          linux: 'linux',
        };

        setPlatform({
          os: platformMap[osType] || 'unknown',
        });
      } catch (error) {
        console.warn('Tauri platform detection failed, falling back to user agent:', error);

        const ua = navigator.userAgent.toLowerCase();
        let detectedOs: OsName = 'unknown';

        if (/win/.test(ua)) {
          detectedOs = 'windows';
        } else if (/mac/.test(ua)) {
          detectedOs = 'macos';
        } else if (/linux/.test(ua)) {
          detectedOs = 'linux';
        }

        setPlatform({
          os: detectedOs,
        });
      }
    };

    detectPlatform();
  }, []);

  return platform;
}
