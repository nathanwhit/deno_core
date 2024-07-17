// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
"use strict";

((window) => {
  const core = Deno.core;
  const {
    op_format_file_name,
    op_apply_source_map,
    op_apply_source_map_filename,
    op_set_call_site_evals,
  } = core.ops;
  const {
    Error,
    ArrayPrototypePush,
    StringPrototypeStartsWith,
    StringPrototypeEndsWith,
    Uint8Array,
    Uint32Array,
  } = window.__bootstrap.primordials;

  const DATA_URL_ABBREV_THRESHOLD = 150; // keep in sync with ./error.rs

  // Keep in sync with `format_file_name` in ./error.rs
  function formatFileName(fileName) {
    if (
      fileName.startsWith("data:") &&
      fileName.length > DATA_URL_ABBREV_THRESHOLD
    ) {
      return op_format_file_name(fileName);
    }
    return fileName;
  }

  // Keep in sync with `cli/fmt_errors.rs`.
  function formatLocation(callSite) {
    if (callSite.isNative) {
      return "native";
    }
    let result = "";
    if (callSite.fileName) {
      result += formatFileName(callSite.fileName);
    } else {
      if (callSite.isEval) {
        if (callSite.evalOrigin == null) {
          throw new Error("assert evalOrigin");
        }
        result += `${callSite.evalOrigin}, `;
      }
      result += "<anonymous>";
    }
    if (callSite.lineNumber !== null) {
      result += `:${callSite.lineNumber}`;
      if (callSite.columnNumber !== null) {
        result += `:${callSite.columnNumber}`;
      }
    }
    return result;
  }

  // Keep in sync with `cli/fmt_errors.rs`.
  function formatCallSiteEval(callSite) {
    let result = "";
    if (callSite.isAsync) {
      result += "async ";
    }
    if (callSite.isPromiseAll) {
      result += `Promise.all (index ${callSite.promiseIndex})`;
      return result;
    }
    const isMethodCall = !(callSite.isToplevel || callSite.isConstructor);
    if (isMethodCall) {
      if (callSite.functionName) {
        if (callSite.typeName) {
          if (
            !StringPrototypeStartsWith(callSite.functionName, callSite.typeName)
          ) {
            result += `${callSite.typeName}.`;
          }
        }
        result += callSite.functionName;
        if (callSite.methodName) {
          if (
            !StringPrototypeEndsWith(callSite.functionName, callSite.methodName)
          ) {
            result += ` [as ${callSite.methodName}]`;
          }
        }
      } else {
        if (callSite.typeName) {
          result += `${callSite.typeName}.`;
        }
        if (callSite.methodName) {
          result += callSite.methodName;
        } else {
          result += "<anonymous>";
        }
      }
    } else if (callSite.isConstructor) {
      result += "new ";
      if (callSite.functionName) {
        result += callSite.functionName;
      } else {
        result += "<anonymous>";
      }
    } else if (callSite.functionName) {
      result += callSite.functionName;
    } else {
      result += formatLocation(callSite);
      return result;
    }

    result += ` (${formatLocation(callSite)})`;
    return result;
  }

  const applySourceMapRetBuf = new Uint32Array(2);
  const applySourceMapRetBufView = new Uint8Array(applySourceMapRetBuf.buffer);

  function prepareStackTrace(error, callSites) {
    const message = error.message !== undefined ? error.message : "";
    const name = error.name !== undefined ? error.name : "Error";
    let stack;
    if (name != "" && message != "") {
      stack = `${name}: ${message}`;
    } else if ((name || message) != "") {
      stack = name || message;
    } else {
      stack = "";
    }
    const callSiteEvals = [];
    for (let i = 0; i < callSites.length; ++i) {
      const v8CallSite = callSites[i];
      const callSite = {
        this: v8CallSite.getThis(),
        typeName: v8CallSite.getTypeName(),
        function: v8CallSite.getFunction(),
        functionName: v8CallSite.getFunctionName(),
        methodName: v8CallSite.getMethodName(),
        fileName: v8CallSite.getFileName(),
        lineNumber: v8CallSite.getLineNumber(),
        columnNumber: v8CallSite.getColumnNumber(),
        evalOrigin: v8CallSite.getEvalOrigin(),
        isToplevel: v8CallSite.isToplevel(),
        isEval: v8CallSite.isEval(),
        isNative: v8CallSite.isNative(),
        isConstructor: v8CallSite.isConstructor(),
        isAsync: v8CallSite.isAsync(),
        isPromiseAll: v8CallSite.isPromiseAll(),
        promiseIndex: v8CallSite.getPromiseIndex(),
      };
      let res = 0;
      if (
        callSite.fileName !== null && callSite.lineNumber !== null &&
        callSite.columnNumber !== null
      ) {
        res = op_apply_source_map(
          callSite.fileName,
          callSite.lineNumber,
          callSite.columnNumber,
          applySourceMapRetBufView,
        );
      }
      if (res >= 1) {
        callSite.lineNumber = applySourceMapRetBuf[0];
        callSite.columnNumber = applySourceMapRetBuf[1];
      }
      if (res >= 2) {
        callSite.fileName = op_apply_source_map_filename();
      }
      // add back the file:// prefix to avoid updating a bunch of deno tests that expect a certain format
      if (callSite.fileName && (callSite.fileName.startsWith("/") || callSite.fileName.startsWith("\\") || callSite.fileName.startsWith("."))) {
        callSite.fileName = "file://" + callSite.fileName;
      }
      ArrayPrototypePush(callSiteEvals, callSite);
      stack += `\n    at ${formatCallSiteEval(callSite)}`;
    }
    op_set_call_site_evals(error, callSiteEvals);
    return stack;
  }

  Error.prepareStackTrace = prepareStackTrace;

// potential solution for bindings issue, causes big perf hit to
// Error.captureStackTrace but doesn't effect `new Error`. Also doesn't fix callsites package. 
//
// const originalCaptureStackTrace = Error.captureStackTrace;
// const originalPrepareStackTrace = Error.prepareStackTrace;

// Error.captureStackTrace = function (err, cons) {
//   const prepareStackTrace = Error.prepareStackTrace;

//   if (prepareStackTrace == originalPrepareStackTrace) {
//     return originalCaptureStackTrace(err, cons);
//   }
//   Error.prepareStackTrace = function (error, stack) {
//     for (let i = 0; i < stack.length; i++) {
//       const frame = stack[i];
//       const newFrame = {
//         getFileName() {
//           let fileName = frame.getFileName();
//           if (fileName.indexOf("file://") === 0) {
//             fileName = fileName.slice(7);
//           }
//           return fileName;
//         },
//         getThis() {
//           return frame.getThis();
//         },
//         getTypeName() {
//           return frame.getTypeName();
//         },
//         getFunction() {
//           return frame.getFunction();
//         },
//         getFunctionName() {
//           return frame.getFunctionName();
//         },
//         getMethodName() {
//           return frame.getMethodName();
//         },
//         getLineNumber() {
//           return frame.getLineNumber();
//         },
//         getColumnNumber() {
//           return frame.getColumnNumber();
//         },
//         getEvalOrigin() {
//           return frame.getEvalOrigin();
//         },
//         isToplevel() {
//           return frame.isToplevel();
//         },
//         isEval() {
//           return frame.isEval();
//         },
//         isNative() {
//           return frame.isNative();
//         },
//         isConstructor() {
//           return frame.isConstructor();
//         },
//         isAsync() {
//           return frame.isAsync();
//         },
//         isPromiseAll() {
//           return frame.isPromiseAll();
//         },
//         getPromiseIndex() {
//           return frame.getPromiseIndex();
//         },
//       };
//       stack[i] = newFrame;
//     }
//     return prepareStackTrace(error, stack.slice(1));
//   };

//   return originalCaptureStackTrace(err, cons);
// };


})(this);
