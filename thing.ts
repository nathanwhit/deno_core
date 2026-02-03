// Copyright 2018-2025 the Deno authors. MIT license.
process.on("exit", () => {
  console.log("exit");
});
console.log(import.meta.url);
