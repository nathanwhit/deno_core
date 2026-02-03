import $ from "dax";

const TIMEOUT_MS = 30_000; // 30 second timeout per test
const SUITE_DIR = "testing/node_tests/suite";
const DCORE_BIN = "./target/release-with-debug/dcore";

await $`cargo build --profile=release-with-debug`;

// Get all test files
const testFiles: string[] = [];
for await (const entry of Deno.readDir(SUITE_DIR)) {
  if (entry.isFile && entry.name.endsWith(".js")) {
    testFiles.push(entry.name);
  }
}
testFiles.sort();

interface TestResult {
  name: string;
  passed: boolean;
  error?: string;
  timedOut?: boolean;
}

const results: TestResult[] = [];

console.log(`Running ${testFiles.length} tests...\n`);

for (const testFile of testFiles) {
  const testPath = `${SUITE_DIR}/${testFile}`;
  process.stdout.write(`Running ${testFile}... `);

  try {
    const result = await $`${DCORE_BIN} ${testPath}`
      .timeout(TIMEOUT_MS)
      .stdout("piped")
      .stderr("piped")
      .noThrow();

    if (result.code === 0) {
      console.log("PASS");
      results.push({ name: testFile, passed: true });
    } else {
      console.log("FAIL");
      const stderr = result.stderr;
      const stdout = result.stdout;
      results.push({
        name: testFile,
        passed: false,
        error: stderr || stdout || `Exit code: ${result.code}`,
      });
    }
  } catch (e) {
    if (e instanceof Error && e.message.includes("timed out")) {
      console.log("TIMEOUT");
      results.push({ name: testFile, passed: false, timedOut: true });
    } else {
      console.log("ERROR");
      results.push({
        name: testFile,
        passed: false,
        error: e instanceof Error ? e.message : String(e),
      });
    }
  }
}

// Print summary
console.log("\n" + "=".repeat(60));
console.log("SUMMARY");
console.log("=".repeat(60));

const passed = results.filter((r) => r.passed).length;
const failed = results.filter((r) => !r.passed).length;
const timedOut = results.filter((r) => r.timedOut).length;

console.log(`Passed: ${passed}/${results.length}`);
console.log(`Failed: ${failed}`);
if (timedOut > 0) {
  console.log(`Timed out: ${timedOut}`);
}

if (failed > 0) {
  console.log("\nFailed tests:");
  for (const result of results) {
    if (!result.passed) {
      console.log(`  - ${result.name}${result.timedOut ? " (timeout)" : ""}`);
    }
  }
}

Deno.exit(failed > 0 ? 1 : 0);
