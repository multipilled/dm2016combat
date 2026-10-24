"""Tests for check_wording.py. Run: python3 -B -m unittest discover -s tools -p "test_*.py" """
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from check_wording import problems_in_text  # noqa: E402

B = "b" "ox"  # the word in pieces, so this file passes the check itself

BLOCKED = [p.format(B=B, b=B.capitalize()) for p in [
    "Copy it with CopyTo{b} first.",
    "copy_to_{B} refuses files over 100 MiB",
    "CopyFrom{b} puts the file on the owner's disk",
    "The exe is at /home/{B}/doom-ref.",
    "Times are {B}-local.",
    "Run the tests on the {B}.",
    "the {B}'s shell has cargo",
    "Copied onto the {B} for reference.",
    "Pull the logs from the {B}.",
    "The {B} clock is set to New York.",
    "A dev {B} with no game install.",
    "Install rustup into the {B}",
]]

ALLOWED = [
    "The player's collision box is 32 wide.",
    "grow the bounding box by r",
    "Demons' hitbox spheres",
    "the player's clip box (slab test)",
    "A text box shows the key name.",
    "The check box toggles the cvar.",
    "the skybox material",
    "let aabb = Aabb::new(lo, hi); let b: BoundingBox = aabb.into();",
    "adds the box's first axis * pm_boxRepulsorScalar",
    "the origin is inside the box's bounds",
    "every player whose clip bounds touch the box pos +- viewShakeDistance",
    "A horizontal line above the box misses",
    "The merged box's 12 edges",
    "RS_BOX: the origin is inside the box (0x1405cbcd0)",
]


class WordingTest(unittest.TestCase):
    def test_blocked(self):
        for s in BLOCKED:
            with self.subTest(s=s):
                self.assertTrue(problems_in_text(s), s)

    def test_allowed(self):
        for s in ALLOWED:
            with self.subTest(s=s):
                self.assertEqual(problems_in_text(s), [], s)

    def test_line_numbers(self):
        hits = problems_in_text("fine\nrun it on the " + B + "\n")
        self.assertEqual([h[0] for h in hits], [2])


if __name__ == "__main__":
    unittest.main()
