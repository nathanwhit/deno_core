// Basic assert module implementation

class AssertionError extends Error {
  actual: unknown;
  expected: unknown;
  operator: string;

  constructor(options: {
    message?: string;
    actual?: unknown;
    expected?: unknown;
    operator?: string;
  }) {
    const message =
      options.message ||
      `${options.actual} ${options.operator} ${options.expected}`;
    super(message);
    this.name = "AssertionError";
    this.actual = options.actual;
    this.expected = options.expected;
    this.operator = options.operator || "==";
  }
}

function ok(value: unknown, message?: string): asserts value {
  if (!value) {
    throw new AssertionError({
      message: message || `Expected truthy value, got ${value}`,
      actual: value,
      expected: true,
      operator: "==",
    });
  }
}

function strictEqual<T>(
  actual: unknown,
  expected: T,
  message?: string
): asserts actual is T {
  if (!Object.is(actual, expected)) {
    throw new AssertionError({
      message:
        message || `Expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
      actual,
      expected,
      operator: "===",
    });
  }
}

function notStrictEqual(
  actual: unknown,
  expected: unknown,
  message?: string
): void {
  if (Object.is(actual, expected)) {
    throw new AssertionError({
      message: message || `Expected values to be strictly unequal`,
      actual,
      expected,
      operator: "!==",
    });
  }
}

function deepStrictEqual(
  actual: unknown,
  expected: unknown,
  message?: string
): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new AssertionError({
      message: message || `Expected deep strict equality`,
      actual,
      expected,
      operator: "deepStrictEqual",
    });
  }
}

function fail(message?: string): never {
  throw new AssertionError({
    message: message || "Failed",
    operator: "fail",
  });
}

function throws(
  fn: () => void,
  errorOrMessage?: RegExp | Function | string,
  message?: string
): void {
  let threw = false;
  try {
    fn();
  } catch (e) {
    threw = true;
    if (typeof errorOrMessage === "function") {
      if (!(e instanceof errorOrMessage)) {
        throw new AssertionError({
          message: message || `Expected error to be instance of ${errorOrMessage.name}`,
          actual: e,
          expected: errorOrMessage,
          operator: "throws",
        });
      }
    } else if (errorOrMessage instanceof RegExp) {
      if (!errorOrMessage.test(String(e))) {
        throw new AssertionError({
          message: message || `Expected error to match ${errorOrMessage}`,
          actual: e,
          expected: errorOrMessage,
          operator: "throws",
        });
      }
    }
  }
  if (!threw) {
    throw new AssertionError({
      message: message || "Expected function to throw",
      operator: "throws",
    });
  }
}

const assert = Object.assign(ok, {
  ok,
  strictEqual,
  notStrictEqual,
  deepStrictEqual,
  fail,
  throws,
  AssertionError,
});

export default assert;
export {
  ok,
  strictEqual,
  notStrictEqual,
  deepStrictEqual,
  fail,
  throws,
  AssertionError,
};
