import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useStore } from '../store';
import type { ScanProgress } from '../types';

/**
 * Progress of the wireless-adb sweep the backend runs when the refresh button
 * is pressed. The last event stays in the store after the sweep ends, so the
 * result line remains visible next to "Updated HH:MM:SS".
 */
export function useScanProgress() {
  const setScanProgress = useStore((s) => s.setScanProgress);

  useEffect(() => {
    const unlisten = listen<ScanProgress>('scan-progress', (event) => {
      setScanProgress(event.payload);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [setScanProgress]);
}
