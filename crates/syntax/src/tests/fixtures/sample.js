/* A synthetic JavaScript sample. */
const path = require("path");

function walk(root, { depth = 1 } = {}) {
  if (depth > 3 || root === null) return [];
  let found = [];
  for (const entry of root.children) {
    found.push(...walk(entry, { depth: depth + 1 }));
  }
  console.log(`walked ${found.length}`);
  return found;
}

const View = () => <section id="main">{walk(this)}</section>;
module.exports = { walk, View, BATCH_SIZE: 64 };
