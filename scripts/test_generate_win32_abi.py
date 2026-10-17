"""Focused ABI slot-width regression tests for generate_win32_abi.py."""

import importlib.util
from pathlib import Path
import unittest


SCRIPT = Path(__file__).with_name("generate_win32_abi.py")
SPEC = importlib.util.spec_from_file_location("generate_win32_abi", SCRIPT)
ABI = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(ABI)


class X86StackWidthTests(unittest.TestCase):
    def test_win32_small_parameters_each_consume_one_dword(self):
        self.assertEqual(ABI.arg_stack_width("value: u8", {}, {}), 4)
        self.assertEqual(ABI.arg_stack_width("value: i16", {}, {}), 4)
        self.assertEqual(ABI.arg_stack_width("value: u32", {}, {}), 4)

    def test_wide_and_pointer_parameters_consume_their_stack_slots(self):
        self.assertEqual(ABI.arg_stack_width("value: u64", {}, {}), 8)
        self.assertEqual(ABI.arg_stack_width("value: *const core::ffi::c_void", {}, {}), 4)

    def test_prototype_cleanup_sums_slots_and_preserves_cdecl(self):
        self.assertEqual(
            ABI.prototype_cleanup_bytes(
                "system",
                ["a: u8", "b: u16", "c: u64", "d: *const core::ffi::c_void"],
                {},
                {},
            ),
            20,
        )
        self.assertEqual(ABI.prototype_cleanup_bytes("C", ["a: unknown"], {}, {}), 0)

    def test_unknown_parameters_remain_unresolved(self):
        self.assertIsNone(ABI.prototype_cleanup_bytes("system", ["a: UnknownType"], {}, {}))


if __name__ == "__main__":
    unittest.main()
