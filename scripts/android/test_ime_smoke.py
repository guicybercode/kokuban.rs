import unittest
import xml.etree.ElementTree as ET

from ime_smoke import callback_counts, center, find_node, node_bounds


class ImeEvidenceTests(unittest.TestCase):
    def test_keys_are_resolved_within_actual_keyboard_package(self):
        root = ET.fromstring('''<hierarchy>
          <node package="com.kokuban.terminal" text="e" bounds="[0,0][500,500]" />
          <node package="keyboard" content-desc="E" enabled="true" bounds="[10,600][70,660]" />
          <node package="keyboard" text="é" bounds="[60,500][120,560]" />
        </hierarchy>''')
        self.assertEqual(center(find_node(root, ["e"], "keyboard")), (40, 630))
        self.assertEqual(center(find_node(root, ["é"], "keyboard")), (90, 530))
        with self.assertRaises(LookupError):
            find_node(root, ["enter"], "keyboard")

    def test_disabled_empty_and_invalid_bounds_do_not_become_taps(self):
        root = ET.fromstring('''<hierarchy>
          <node package="keyboard" text="x" enabled="false" bounds="[0,0][50,50]" />
          <node package="keyboard" text="x" bounds="[0,0][0,0]" />
        </hierarchy>''')
        with self.assertRaises(LookupError):
            find_node(root, ["x"], "keyboard")
        with self.assertRaises(ValueError):
            node_bounds(ET.fromstring('<node bounds="not bounds"/>'))

    def test_keyboard_popup_is_found_in_a_separate_interactive_window(self):
        root = ET.fromstring('''<displays><display><windows>
          <window><hierarchy><node package="app" text="e" bounds="[0,0][500,500]" /></hierarchy></window>
          <window><hierarchy><node package="keyboard" text="e" bounds="[10,600][70,660]" /></hierarchy></window>
          <window><hierarchy><node package="keyboard" text="é" bounds="[60,500][120,560]" /></hierarchy></window>
        </windows></display></displays>''')
        self.assertEqual(center(find_node(root, ["é"], "keyboard")), (90, 530))

    def test_empty_preedit_does_not_prove_composition(self):
        log = "\n".join([
            "ime callback operation=preedit nonempty=false",
            "ime callback operation=commit nonempty=true",
            "ime callback operation=preedit nonempty=true",
            "ime callback operation=finish nonempty=false",
        ])
        self.assertEqual(callback_counts(log), {"preedit": 1, "commit": 1})
        self.assertEqual(callback_counts("adb input text café"), {"preedit": 0, "commit": 0})


if __name__ == "__main__":
    unittest.main()
