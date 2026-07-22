#!/usr/bin/env node

import { releaseMatrix } from "../src/platforms.js";

process.stdout.write(`${JSON.stringify(releaseMatrix())}\n`);
