"use strict";

const path = require("path");

function resolveAll(root, names) {
  return names.map(function join(name) {
    return path.join(root, name);
  });
}

var cache = {};

function remember(key, compute) {
  if (!(key in cache)) {
    cache[key] = compute(key);
  }
  return cache[key];
}

module.exports.resolveAll = resolveAll;
module.exports = { resolveAll, remember };
