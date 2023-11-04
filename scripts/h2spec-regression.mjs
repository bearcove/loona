import { Octokit } from "octokit";

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

console.log("output summary: ", JSON.stringify(http2Check.output.summary));
let http2CheckResults = extractTestResults(http2Check.output.summary);
console.log(JSON.stringify(http2CheckResults));

function extractTestResults(resultString) {
  // example input:
  // **94** tests were completed in **NaNms** with **70** passed, **24** failed and **0** skipped
  // example output:
  // {
  //   total: 94,
  //   passed: 70,
  //   failed: 24,
  //   skipped: 0,
  // }
  // using a regexp:
  const regex =
    /[*][*](\d+)[*][*] tests were completed in [*][*](\w+)ms[*][*] with [*][*](\d+)[*][*] passed, [*][*](\d+)[*][*] failed and [*][*](\d+)[*][*] skipped/g;
  let m = regex.exec(resultString);
  return {
    total: parseInt(m[1]),
    passed: parseInt(m[3]),
    failed: parseInt(m[4]),
    skipped: parseInt(m[5]),
  };
}
