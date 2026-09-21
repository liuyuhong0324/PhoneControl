import { describe, it, expect } from 'vitest';
import { scanProgressText, scanProgressDetail } from './scanText';
import type { ScanProgress } from '../types';

function progress(overrides: Partial<ScanProgress> = {}): ScanProgress {
  return {
    daemonHost: '127.0.0.1',
    daemonPort: 5037,
    segment: '192.168.1.0/24',
    scanned: 0,
    total: 254,
    found: [],
    connected: 0,
    alreadyConnected: 0,
    done: false,
    error: null,
    ...overrides,
  };
}

describe('scanProgressText', () => {
  it('shows nothing before any sweep has run', () => {
    expect(scanProgressText(null)).toBeNull();
  });

  it('counts through the sweep while it runs', () => {
    const text = scanProgressText(progress({ scanned: 128, found: ['192.168.1.21:5555'] }));

    expect(text).toBe('Scanning 192.168.1.0/24 128/254 · found 1');
  });

  it('summarises how many were found and attached when it finishes', () => {
    const text = scanProgressText(
      progress({ done: true, scanned: 254, found: ['a:5555', 'b:5555'], connected: 2 }),
    );

    expect(text).toBe('Scan: 2 found, 2 attached');
  });

  it('reports a segment it could not read', () => {
    const text = scanProgressText(
      progress({ done: true, total: 0, segment: 'nonsense', error: "cannot read segment 'nonsense'" }),
    );

    expect(text).toBe("Scan failed: cannot read segment 'nonsense'");
  });

  it('stays quiet for a finished sweep with nothing to say', () => {
    expect(scanProgressText(progress({ done: true, total: 0 }))).toBeNull();
  });

  it('separates what was found from what was already connected', () => {
    const text = scanProgressText(
      progress({ done: true, scanned: 233, found: ['a:5555'], connected: 1, alreadyConnected: 21 }),
    );

    expect(text).toBe('Scan: 1 found, 1 attached · 21 already connected');
  });

  /// The usual refresh now that the fleet stays connected: everything in the
  /// segment is on the daemon already, so nothing is probed at all.
  it('says nothing was new when the whole segment was already connected', () => {
    const text = scanProgressText(progress({ done: true, total: 0, alreadyConnected: 21 }));

    expect(text).toBe('21 already connected · nothing new');
  });
});

describe('scanProgressDetail', () => {
  it('lists the addresses that answered', () => {
    const detail = scanProgressDetail(
      progress({ done: true, found: ['192.168.1.21:5555', '192.168.1.22:5555'] }),
    );

    expect(detail).toBe('192.168.1.21:5555\n192.168.1.22:5555');
  });

  it('says so when nothing answered', () => {
    expect(scanProgressDetail(progress({ done: true, scanned: 254 }))).toBe('no new devices answered');
  });

  it('says how many were skipped because they are already connected', () => {
    const probed = scanProgressDetail(
      progress({ done: true, scanned: 233, total: 233, alreadyConnected: 21 }),
    );
    expect(probed).toBe('no new devices answered\n21 already connected, not probed');

    // Nothing was probed at all: the only thing to say is what is already there.
    const none = scanProgressDetail(progress({ done: true, total: 0, alreadyConnected: 21 }));
    expect(none).toBe('21 already connected, not probed');
  });

  it('appends the reason a found device could not be attached', () => {
    const detail = scanProgressDetail(
      progress({
        done: true,
        found: ['192.168.1.21:5555'],
        error: '192.168.1.21:5555: adb command failed: failed to connect',
      }),
    );

    expect(detail).toContain('192.168.1.21:5555');
    expect(detail).toContain('failed to connect');
  });

  it('shows nothing while the sweep is still running', () => {
    expect(scanProgressDetail(progress({ scanned: 20 }))).toBeUndefined();
  });
});
