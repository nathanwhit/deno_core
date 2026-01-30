// Copyright 2018-2025 the Deno authors. MIT license.
export class ERR_ILLEGAL_CONSTRUCTOR extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_ILLEGAL_CONSTRUCTOR";
  }
}

export class ERR_INVALID_ARG_TYPE extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_INVALID_ARG_TYPE";
  }
}

export class ERR_INVALID_ARG_VALUE extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_INVALID_ARG_VALUE";
  }
}

export class AbortError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AbortError";
  }
}

export class ERR_UNHANDLED_ERROR extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_UNHANDLED_ERROR";
  }
}

export class ERR_MISSING_ARGS extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_MISSING_ARGS";
  }
}

export class ERR_OUT_OF_RANGE extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_OUT_OF_RANGE";
  }
}

export class ERR_METHOD_NOT_IMPLEMENTED extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_METHOD_NOT_IMPLEMENTED";
  }
}

export class ERR_STREAM_PUSH_AFTER_EOF extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_STREAM_PUSH_AFTER_EOF";
  }
}

export class ERR_STREAM_UNSHIFT_AFTER_END_EVENT extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_STREAM_UNSHIFT_AFTER_END_EVENT";
  }
}

export class ERR_STREAM_ALREADY_FINISHED extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_STREAM_ALREADY_FINISHED";
  }
}

export class ERR_STREAM_DESTROYED extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_STREAM_DESTROYED";
  }
}

export class ERR_UNKNOWN_ENCODING extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ERR_UNKNOWN_ENCODING";
  }
}

export default {
  AbortError,
  codes: {
    ERR_ILLEGAL_CONSTRUCTOR,
    ERR_INVALID_ARG_TYPE,
    ERR_INVALID_ARG_VALUE,
    ERR_UNHANDLED_ERROR,
    ERR_MISSING_ARGS,
    ERR_OUT_OF_RANGE,
    ERR_METHOD_NOT_IMPLEMENTED,
    ERR_STREAM_PUSH_AFTER_EOF,
    ERR_STREAM_UNSHIFT_AFTER_END_EVENT,
    ERR_STREAM_ALREADY_FINISHED,
    ERR_STREAM_DESTROYED,
    ERR_UNKNOWN_ENCODING,
  },
};
