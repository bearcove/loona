import { Octokit } from "octokit";
import { XMLParser } from "fast-xml-parser";
import fs from "fs/promises";

let xmlParser = new XMLParser({
  ignoreAttributes: false,
});
let http2Result = xmlParser.parse(await fs.readFile("target/h2spec-http2.xml"));
console.log(JSON.stringify(http2Result, null, 2));

let results = {
  total: 0,
  passed: 0,
  failed: 0,
  skipped: 0,
};

for (const ts of http2Result.testsuites.testsuite) {
  let tests = parseInt(ts["@_tests"], 10);
  let failures = parseInt(ts["@_failures"], 10);
  let errors = parseInt(ts["@_errors"], 10);
  let skipped = parseInt(ts["@_skipped"], 10);

  results.total += tests;
  results.failed += failures + errors;
  results.skipped += skipped;
  results.passed += tests - (failures + errors + skipped);
}
console.log(JSON.stringify(results, null, 2));

process.exit(0);

const octokit = new Octokit({
  auth: process.env.TOKEN,
});

let checks = await octokit.rest.checks.listForRef({
  owner: "hapsoc",
  repo: "fluke",
  ref: "main",
});

let http2Check = checks.data.check_runs.find((c) => c.name == "h2spec-http2");
if (!http2Check) {
  console.log("No h2spec-http2 check found");
  process.exit(0);
}

let http2CheckResults = extractTestResultsFromCheckSummary(
  http2Check.output.summary,
);
console.log(JSON.stringify(http2CheckResults));

function extractTestResultsFromCheckSummary(resultString) {
  // example input:
  //   **94** tests were completed in **NaNms** with **70** passed, **24** failed and **0** skipped
  // example output:
  //   { total: 94, passed: 70, failed: 24, skipped: 0 }
  const regex =
    /[*][*](\d+)[*][*] tests were completed in [*][*](\w+)ms[*][*] with [*][*](\d+)[*][*] passed, [*][*](\d+)[*][*] failed and [*][*](\d+)[*][*] skipped/g;
  let m = regex.exec(resultString);
  return {
    total: parseInt(m[1], 10),
    passed: parseInt(m[3], 10),
    failed: parseInt(m[4], 10),
    skipped: parseInt(m[5], 10),
  };
}
