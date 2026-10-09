#!/usr/bin/env python3
"""Exercise exclusion, nesting, pool allocation and cancellation with real processes."""
import os
from pathlib import Path
import select
import subprocess
import sys
import unittest
import uuid

WRAPPER = str(Path(__file__).with_name('with-resource.py'))

class Resources(unittest.TestCase):
    def setUp(self):
        self.resource = 'test-' + uuid.uuid4().hex
        self.children = []

    def tearDown(self):
        for child in self.children:
            if child.poll() is None:
                child.terminate()
            child.communicate(timeout=5)

    def start(self, resource, command=None, **environment):
        command = command or [sys.executable, '-u', '-c',
                              'import os,sys; print(os.environ.get("ANDROID_SERIAL", "ready"), flush=True); sys.stdin.readline()']
        child = subprocess.Popen([sys.executable, WRAPPER, resource, *command],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 text=True, env=dict(os.environ, **environment))
        self.children.append(child)
        return child

    def read(self, child):
        self.assertTrue(select.select([child.stdout], [], [], 5)[0], 'child did not acquire its resource')
        return child.stdout.readline().strip()

    def test_serializes_and_releases_after_cancellation(self):
        first = self.start(self.resource)
        self.read(first)
        second = self.start(self.resource)
        self.assertEqual(second.stderr.readline().strip(), 'waiting for nori resource: ' + self.resource)
        self.assertFalse(select.select([second.stdout], [], [], 0)[0])
        first.terminate()
        first.wait(timeout=5)
        self.read(second)

    def test_independent_resources_run_together(self):
        first = self.start(self.resource)
        self.read(first)
        self.read(self.start(self.resource + '-other'))
        self.assertIsNone(first.poll())

    def test_nested_resource_does_not_deadlock(self):
        child = self.start(self.resource, [sys.executable, WRAPPER, self.resource,
                                          sys.executable, '-c', 'print("nested")'])
        self.assertEqual(child.communicate(timeout=5)[0].strip(), 'nested')
        self.assertEqual(child.returncode, 0)

    def test_pool_chooses_available_device(self):
        first = self.start('device:' + self.resource)
        self.read(first)
        available = self.resource + '-other'
        second = self.start('device', NORI_E2E_DEVICES=self.resource + ',' + available)
        self.assertEqual(self.read(second), available)

if __name__ == '__main__':
    unittest.main()
