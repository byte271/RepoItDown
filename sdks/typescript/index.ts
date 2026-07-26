/**
 * RepoItDown TypeScript SDK
 *
 * Thin wrapper around the `repoitdown-cli` binary. Spawns the CLI as a
 * child process and returns the Markdown topology as a string.
 *
 * For a native Node.js addon (zero process-spawn overhead), a future
 * napi-rs binding is planned. This CLI-wrapper approach works today
 * with any installed `repoitdown` binary.
 *
 * @example
 * ```ts
 * import { repoitdown } from 'repoitdown';
 *
 * const output = await repoitdown({
 *   repoPath: '.',
 *   mode: 'architect',
 *   maxTokens: 8000,
 * });
 * console.log(output);
 * ```
 */

import { spawn } from 'node:child_process';
import { resolve } from 'node:path';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** Valid processing modes. */
export type RepoItDownMode = 'dump' | 'explore' | 'architect' | 'task';

/** Options for a RepoItDown run. */
export interface RepoItDownOptions {
  /** Absolute or relative path to the repository root. Required. */
  repoPath: string;

  /**
   * Processing mode. Default: `'dump'`.
   *
   * - `dump` — full source concatenation (Phase 1 behaviour)
   * - `explore` — full source + Contract View of exported symbols
   * - `architect` — skeletonized files with PageRank hubs in full source
   * - `task` — BM25 query targets in full, k-hop deps skeletonized
   */
  mode?: RepoItDownMode;

  /**
   * Maximum output tokens. Serves as slicing budget for `architect` and `task`.
   */
  maxTokens?: number;

  /**
   * Natural-language query for `task` mode. Required when `mode` is `'task'`.
   */
  query?: string;

  /** Disable collapsible HTML details (plain Markdown output). */
  noCollapse?: boolean;

  /** Path to the `repoitdown` binary. Default: `'repoitdown'` (from PATH). */
  binaryPath?: string;
}

/** Result of a RepoItDown run. */
export interface RepoItDownResult {
  /** The rendered Markdown topology. */
  output: string;
  /** Exit code (0 = success). */
  exitCode: number;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/**
 * Run RepoItDown on a repository and return the Markdown topology.
 *
 * Spawns the `repoitdown` CLI as a child process. The binary must be
 * installed and available on PATH (or specified via `binaryPath`).
 */
export function repoitdown(options: RepoItDownOptions): Promise<RepoItDownResult> {
  return new Promise((resolvePromise, reject) => {
    const {
      repoPath,
      mode = 'dump',
      maxTokens,
      query,
      noCollapse = false,
      binaryPath = 'repoitdown',
    } = options;

    const args: string[] = [resolve(repoPath), '--mode', mode];

    if (maxTokens !== undefined) {
      args.push('--max-tokens', String(maxTokens));
    }

    if (mode === 'task') {
      if (!query || query.trim().length === 0) {
        reject(new Error("mode 'task' requires a non-empty 'query' option"));
        return;
      }
      args.push('--query', query);
    }

    if (noCollapse) {
      args.push('--no-collapse');
    }

    const child = spawn(binaryPath, args, {
      stdio: ['ignore', 'pipe', 'pipe'],
    });

    let stdout = '';
    let stderr = '';

    child.stdout.on('data', (chunk: Buffer) => {
      stdout += chunk.toString();
    });

    child.stderr.on('data', (chunk: Buffer) => {
      stderr += chunk.toString();
    });

    child.on('close', (code) => {
      if (code === 0) {
        resolvePromise({ output: stdout, exitCode: 0 });
      } else {
        reject(
          new Error(
            `repoitdown exited with code ${code}${stderr ? ': ' + stderr.trim() : ''}`,
          ),
        );
      }
    });

    child.on('error', (err) => {
      reject(new Error(`failed to spawn repoitdown: ${err.message}`));
    });
  });
}

/**
 * Run RepoItDown and return only the output string (throws on error).
 *
 * Convenience wrapper around `repoitdown()`.
 */
export async function repoitdownToString(options: RepoItDownOptions): Promise<string> {
  const result = await repoitdown(options);
  return result.output;
}
