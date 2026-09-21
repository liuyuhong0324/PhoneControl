import { describe, it, expect } from 'vitest';
import { isLocalServer } from './servers';

const srv = (host: string, port: number) => ({ id: 'x', host, port, enabled: true });

describe('isLocalServer', () => {
  it('recognises the daemon the app starts itself', () => {
    expect(isLocalServer(srv('127.0.0.1', 5037))).toBe(true);
    expect(isLocalServer(srv('localhost', 5037))).toBe(true);
  });

  it('leaves server of the user alone', () => {
    // Same port, other machine: the user added it, so it stays theirs.
    expect(isLocalServer(srv('192.168.101.1', 5037))).toBe(false);
    expect(isLocalServer(srv('127.0.0.1', 5555))).toBe(false);
  });
});
