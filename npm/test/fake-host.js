'use strict';

// Preloaded with `node --require` so the shim, unmodified, believes it runs on
// the target named by FAKE_PLATFORM, FAKE_ARCH and FAKE_LIBC.
const Module = require('node:module');

const { FAKE_PLATFORM, FAKE_ARCH, FAKE_LIBC } = process.env;

Object.defineProperty(process, 'platform', { value: FAKE_PLATFORM });
Object.defineProperty(process, 'arch', { value: FAKE_ARCH });

const load = Module._load;
Module._load = function (request, ...rest) {
  if (request === 'detect-libc') {
    return {
      GLIBC: 'glibc',
      MUSL: 'musl',
      familySync: () => (FAKE_LIBC === 'unknown' ? null : FAKE_LIBC),
    };
  }
  return load.call(this, request, ...rest);
};
