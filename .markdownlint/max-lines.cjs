// Custom markdownlint rule: cap a file's total line count.
"use strict";

module.exports = {
  names: ["max-lines"],
  description: "File exceeds the maximum number of lines",
  tags: ["length"],
  parser: "none",
  function: (params, onError) => {
    const max = Number(params.config.maximum ?? 100);
    // A trailing newline yields one empty last entry; don't count it.
    const lines = params.lines.length -
      (params.lines[params.lines.length - 1] === "" ? 1 : 0);
    if (lines > max) {
      onError({
        lineNumber: max + 1,
        detail: `Expected: <= ${max} lines; Actual: ${lines}`,
      });
    }
  },
};
