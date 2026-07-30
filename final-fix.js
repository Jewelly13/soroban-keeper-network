const fs = require('fs');

// 1. Remove the duplicate CONFIG declaration in index.js
let idx = fs.readFileSync('examples/keeper-bot/index.js', 'utf8');
idx = idx.replace(/let CONFIG;\s*let CONFIG;/g, 'let CONFIG;');
fs.writeFileSync('examples/keeper-bot/index.js', idx);

// 2. Remove the unused variable assignments in retry.test.js
let rty = fs.readFileSync('examples/keeper-bot/test/retry.test.js', 'utf8');
rty = rty.replace(/const originalConfig = .*/g, '');
rty = rty.replace(/_?originalConfig = .*/g, '');
rty = rty.replace(/const _keeper = require/g, 'require');
fs.writeFileSync('examples/keeper-bot/test/retry.test.js', rty);

console.log("✅ Final cleanup complete!");
