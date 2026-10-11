/**
 * The slice of `node:net` this repository uses.
 *
 * Declared here because `@types/node` is not a dependency: Muster's frontend targets a webview,
 * and adding Node's type surface to the whole app to typecheck one shipped extension file would
 * typecheck a great deal of code that runs nowhere near a terminal.
 *
 * Only what `src/pi/muster-bridge.ts` calls is declared, so anything else it reaches for is a
 * compile error rather than a silent `any` in a file that runs inside somebody's agent.
 */
declare module "node:net" {
  export interface Socket extends NodeJS.EventEmitter {
    /** Buffers the write when the connection is not up yet; nothing to await. */
    write(payload: string): boolean;
    /** Flushes buffered writes. A socket has nothing pending, so this resolves immediately. */
    end(): void;
    /** Closes the socket immediately, discarding anything buffered. */
    destroy(): void;
    setNoDelay(enable: boolean): void;
    readonly destroyed: boolean;
  }

  export interface NetConnectOptions {
    host?: string;
    port?: number;
  }

  export function connect(options: NetConnectOptions): Socket;
}

/**
 * The part of Node's global scope this file uses, spelled out for the same reason as `node:net`.
 */
declare const Buffer: {
  byteLength(value: string): number;
};

declare namespace NodeJS {
  interface EventEmitter {
    on(event: string, listener: (...args: never[]) => void): this;
  }
}
