import * as path from 'node:path';

/**
 * The binary under test: `$CODEGRAPH_BIN` (the runner sets it), else this
 * repository's release build. The shim runs from `parity/.work/src`.
 */
export function binary(): string {
  const configured = process.env.CODEGRAPH_BIN;
  if (configured) return configured;
  const exe = process.platform === 'win32' ? 'codegraph.exe' : 'codegraph';
  return path.resolve(__dirname, '..', '..', '..', 'target', 'release', exe);
}

/** The environment every run of the binary gets: no telemetry, no update check. */
export function binaryEnv(): NodeJS.ProcessEnv {
  return { ...process.env, DO_NOT_TRACK: '1', CODEGRAPH_NO_UPDATE_CHECK: '1', CODEGRAPH_TELEMETRY: '0' };
}
