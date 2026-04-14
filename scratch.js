const fs = require('fs');
const js = fs.readFileSync('node_modules/webtorrent/dist/webtorrent.min.js', 'utf8');
const { JSDOM } = require('jsdom');
// try to simulate browser environment
