import { nop } from "checkin:object";
// deno-lint-ignore no-explicit-any
export function do_not_optimize(v: any) {
  $._ = v;
  return v;
}
export const $ = {
  _: null,
  __() {
    return print($._);
  },
};

export const print = (() => {
  if (globalThis.console?.log) return globalThis.console.log;
  if (globalThis.print && !globalThis.document) return globalThis.print;

  return () => {
    throw new Error("no print function available");
  };
})();

function formatTime(time: number) {
  let unit = "ms";
  if (time < 0.0001) {
    time *= 1000;
    unit = "us";
  }
  if (time < 0.0001) {
    time *= 1000;
    unit = "ns";
  }
  return `${time} ${unit}`;
}

export interface BenchOptions {
  warmup?: number;
  iterations?: number;
  innerIterations?: number;
}
export function bench(
  fnOrName: string | (() => string),
  fnOrOptions: BenchOptions | (() => void),
  maybeOptions?: BenchOptions,
) {
  const name = typeof fnOrName === "function" ? fnOrName.name : fnOrName;
  const fn = typeof fnOrName === "function" ? fnOrName : fnOrOptions;
  const options = typeof fnOrOptions === "function"
    ? maybeOptions
    : fnOrOptions;
  const warmup = options?.warmup ?? 10;
  const iterations = options?.iterations ?? 100_000_000;
  const innerIterations = options?.innerIterations ?? 1;
  for (let i = 0; i < warmup; i++) {
    nop(fn)();
  }

  const times = [];
  for (let i = 0; i < iterations; i++) {
    const start = performance.now();
    for (let j = 0; j < innerIterations; j++) {
      nop(fn)();
    }
    const end = performance.now();
    times.push((end - start) / innerIterations);
  }

  const total = times.reduce((a, b) => a + b, 0);
  const average = total / times.length;

  let min = Infinity;
  let max = -Infinity;
  let i = 0;
  for (const time of times) {
    if (time === 0) {
      console.log(`Time is 0 at index ${i}`);
    }
    if (time < min) {
      min = time;
    }
    if (time > max) {
      max = time;
    }
    i++;
  }
  const median = times.sort((a, b) => a - b)[Math.floor(times.length / 2)];

  print(
    `${name}: ${formatTime(average)} (min: ${formatTime(min)}, max: ${
      formatTime(max)
    }, median: ${formatTime(median)})`,
  );
}
