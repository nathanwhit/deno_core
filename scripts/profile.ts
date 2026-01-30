// Copyright 2018-2025 the Deno authors. MIT license.
import $ from "jsr:@david/dax";
import { stdout } from "node:process";

await $`cargo build --profile=release-with-debug`;

await using child = new Deno.Command("flamey", {
  args: [
    "--forward-sigint",
    "--thread",
    "dcore",
    "--",
    import.meta.resolve("../target/release-with-debug/dcore").replace(
      "file://",
      "",
    ),
    import.meta.resolve("../http-server.ts").replace("file://", ""),
  ],
  stdout: "piped",
  stderr: "piped",
})
  .spawn();

console.log("Running benchmark");
const output =
  await $`oha http://localhost:3000 --disable-compression -n 2m --output-format=json`
    .stdout("piped")
    .stderr("piped");

interface OhaOutput {
  summary: {
    successRate: number;
    total: number;
    slowest: number;
    fastest: number;
    average: number;
    requestsPerSec: number;
    totalData: number;
    sizePerRequest: number;
    sizePerSec: number;
  };
}
console.log("Done");
const ohaOutput = JSON.parse(output.stdout) as OhaOutput;
console.log(ohaOutput.summary);
console.log("Requests per second: ", ohaOutput.summary.requestsPerSec);
console.log("Total time: ", ohaOutput.summary.total);
console.log("Total data: ", ohaOutput.summary.totalData);
console.log("Size per request: ", ohaOutput.summary.sizePerRequest);
console.log("Size per second: ", ohaOutput.summary.sizePerSec);
console.log("Success rate: ", ohaOutput.summary.successRate);
child.kill("SIGINT");

const result = await child.output();
const flameyOutput = new TextDecoder().decode(result.stdout);
console.log("Profiling output: ");
console.log(flameyOutput);
