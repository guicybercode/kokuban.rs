#!/usr/bin/env python3
"""Generate a conservative boundary classifier from the exact locked crate.

No network, Cargo, Rust compilation, or timing workloads are invoked.
--check regenerates in memory, exhaustively audits the Unicode scalar domain,
and requires byte-identical checked-in outputs without writing them.
"""
from array import array
from bisect import bisect_right
from pathlib import Path
import os
import argparse
import hashlib
import json
import re
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
VERSION = '1.13.3'
PACKAGE_SHA = 'c6f5d3c3b1bf09027a88a6bc961fc00497d651009560b5463668dc81b0fa87a8'
TABLE_SHA = '089c221918a60e8672ed8eda73d27f195dcc404f91b9e71b57c0b5e348e7aeef'
UNICODE_VERSION = (17, 0, 0)
EXPECTED_RANGES = 1618
DOMAIN = 0x110000
PAGE_SIZE = 64
ATTACH = {'GC_Extend', 'GC_ZWJ', 'GC_SpacingMark'}
CONTROL = {'GC_Control', 'GC_CR', 'GC_LF'}
PREFIX = 'src/grid/'

def sha(data):
    return hashlib.sha256(data).hexdigest()

def category_at(cp, starts, ranges):
    # Independent range lookup; generation below fills disjoint dense intervals.
    index = bisect_right(starts, cp) - 1
    if index >= 0 and cp <= ranges[index][1]:
        return ranges[index][2]
    return 'GC_Any'

def rust_predicate(name, ranges):
    patterns = []
    for lo, hi, _ in ranges:
        left = "'\\u{" + format(lo, 'x') + "}'"
        right = "'\\u{" + format(hi, 'x') + "}'"
        patterns.append(left if lo == hi else left + '..=' + right)
    return '#[inline]\nfn ' + name + '(c: char) -> bool {\n    matches!(c,\n        ' + '\n        | '.join(patterns) + '\n    )\n}\n'

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--crate-path', type=Path,
                        help='Exact .crate archive; defaults to the local Cargo registry cache')
    args = parser.parse_args()
    if not __debug__:
        raise SystemExit('Verification requires Python without -O or PYTHONOPTIMIZE')
    lock = tomllib.loads((ROOT / 'Cargo.lock').read_text())
    packages = [p for p in lock['package'] if p['name'] == 'unicode-segmentation']
    assert len(packages) == 1, 'expected one locked unicode-segmentation package'
    package = packages[0]
    assert package['version'] == VERSION, 'locked crate version requires reviewed table regeneration'
    assert package['checksum'] == PACKAGE_SHA, 'locked crate checksum differs from the audited source'
    assert package['source'] == 'registry+https://github.com/rust-lang/crates.io-index'
    manifest = tomllib.loads((ROOT / 'Cargo.toml').read_text())
    assert manifest['dependencies']['unicode-segmentation'] == '=' + VERSION, 'exact dependency pin is required'
    if args.crate_path is None:
        cargo_home = Path(os.environ.get('CARGO_HOME') or Path.home() / '.cargo').expanduser()
        archives = sorted((cargo_home / 'registry/cache').glob('*/unicode-segmentation-' + VERSION + '.crate'))
        args.crate_path = next((p for p in archives if sha(p.read_bytes()) == PACKAGE_SHA), None)
        if args.crate_path is None:
            raise SystemExit('Verified crate archive missing. Run cargo fetch --locked, or pass --crate-path ARCHIVE.')
    else:
        args.crate_path = args.crate_path.expanduser()
    package_bytes = args.crate_path.read_bytes()
    assert sha(package_bytes) == PACKAGE_SHA, 'crate archive does not match locked checksum'
    inputs = {}
    with tarfile.open(args.crate_path, 'r:gz') as package:
        for name in ['src/tables.rs', 'src/grapheme.rs', 'tests/testdata/mod.rs',
                     'LICENSE-MIT', 'COPYRIGHT']:
            member = package.getmember('unicode-segmentation-' + VERSION + '/' + name)
            assert member.isfile()
            inputs[name] = package.extractfile(member).read()
    assert sha(inputs['src/tables.rs']) == TABLE_SHA
    source = inputs['src/tables.rs'].decode()
    version_match = re.search(r'pub const UNICODE_VERSION: \(u64, u64, u64\) = \((\d+), (\d+), (\d+)\);', source)
    assert version_match and tuple(map(int, version_match.groups())) == UNICODE_VERSION
    table_match = re.search(r'const grapheme_cat_table: &\[\(char, char, GraphemeCat\)\] = &\[(.*?)\n    \];', source, re.S)
    assert table_match
    body = table_match.group(1)
    entry = re.compile(r"\('\\u\{([0-9a-fA-F]+)\}',\s*'\\u\{([0-9a-fA-F]+)\}',\s*(GC_[A-Za-z_]+)\)")
    assert not entry.sub('', body).replace(',', '').strip(), 'unparsed table content'
    ranges = [(int(lo, 16), int(hi, 16), cat) for lo, hi, cat in entry.findall(body)]
    assert len(ranges) == EXPECTED_RANGES
    assert all(0 <= lo <= hi < DOMAIN for lo, hi, _ in ranges)
    assert all(previous[1] < current[0] for previous, current in zip(ranges, ranges[1:]))
    enum_match = re.search(r'pub enum GraphemeCat \{(.*?)\n    \}', source, re.S)
    assert enum_match
    enum_body = enum_match.group(1)
    category_names = re.findall(r'\bGC_[A-Za-z_]+\b', enum_body)
    assert len(category_names) == len(set(category_names)) == 16
    assert not re.sub(r'\bGC_[A-Za-z_]+\b', '', enum_body).replace(',', '').strip()
    assert all(cat in category_names for _, _, cat in ranges)

    # 0 = unknown, 1 = GC_Any, 2 = attaching categories. A page is usable only
    # when every code point in that page has the same nonzero semantic class.
    dense_class = array('B', [1]) * DOMAIN
    for lo, hi, cat in ranges:
        value = 1 if cat == 'GC_Any' else 2 if cat in ATTACH else 0
        dense_class[lo:hi + 1] = array('B', [value]) * (hi - lo + 1)
    pages = []
    for start in range(0, DOMAIN, PAGE_SIZE):
        values = dense_class[start:start + PAGE_SIZE]
        pages.append(values[0] if values[0] != 0 and all(v == values[0] for v in values) else 0)
    words = [sum(pages[start + i] << (2 * i) for i in range(32))
             for start in range(0, len(pages), 32)]
    assert len(words) == 544
    prepend_ranges = [r for r in ranges if r[2] == 'GC_Prepend']
    control_ranges = [r for r in ranges if r[2] in CONTROL]
    assert len(prepend_ranges) == 15 and len(control_ranges) == 21

    starts = [r[0] for r in ranges]
    coverage = {'valid_scalars_checked': 0, 'other': 0, 'attach': 0, 'fallback': 0,
                'prepend_scalars': 0, 'control_cr_lf_scalars': 0}
    for cp in range(DOMAIN):
        if 0xD800 <= cp <= 0xDFFF:
            continue
        cat = category_at(cp, starts, ranges)
        page = cp >> 6
        packed = (words[page >> 5] >> ((page & 31) * 2)) & 3
        value = 1 if 0x20 <= cp <= 0x7E else packed
        assert packed == pages[page]
        assert value in (0, 1, 2)
        if value == 1:
            assert cat == 'GC_Any'
        elif value == 2:
            assert cat in ATTACH
        assert any(lo <= cp <= hi for lo, hi, _ in prepend_ranges) == (cat == 'GC_Prepend')
        assert any(lo <= cp <= hi for lo, hi, _ in control_ranges) == (cat in CONTROL)
        coverage['valid_scalars_checked'] += 1
        coverage[{0: 'fallback', 1: 'other', 2: 'attach'}[value]] += 1
        coverage['prepend_scalars'] += cat == 'GC_Prepend'
        coverage['control_cr_lf_scalars'] += cat in CONTROL
    assert coverage['valid_scalars_checked'] == 1112064

    original_notice = source.split('// NOTE:', 1)[0]
    header = original_notice + '// Generated from the checksum-verified unicode-segmentation 1.13.3 crate.\n'
    header += '// Derived files are distributed under the MIT option; see THIRD_PARTY_LICENSES.\n'
    header += '// Do not edit: run python3 scripts/generate-grapheme-tables.py [--check].\n'
    data = header + 'const GENERATED_UNICODE_VERSION: (u64, u64, u64) = (17, 0, 0);\n'
    data += 'const SUCCESSOR_PAGES: [u64; 544] = [\n'
    for start in range(0, len(words), 4):
        data += '    ' + ', '.join('0x' + format(word, '016x') for word in words[start:start + 4]) + ',\n'
    data += '];\n\n' + rust_predicate('is_prepend', prepend_ranges)
    data += '\n' + rust_predicate('is_control_cr_lf', control_ranges)

    reference = header + '#[allow(non_camel_case_types)]\n#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n'
    reference += 'enum OracleCategory {\n    ' + ',\n    '.join(category_names) + ',\n}\n'
    reference += 'use OracleCategory::*;\nconst REFERENCE_RANGES: &[(char, char, OracleCategory)] = &[' + body + '\n];\n'
    corpus_source = inputs['tests/testdata/mod.rs'].decode()
    corpus_parts = []
    corpus_case_counts = {}
    for name, arity in [('TEST_SAME', 2), ('TEST_DIFF', 3)]:
        pattern = r'    pub const ' + name + r': .*? = &\[.*?\n    \];'
        matched = re.search(pattern, corpus_source, re.S)
        assert matched
        block = matched.group()
        # Every case starts with a Rust tuple whose first member is a string.
        count = len(re.findall(r'\(\s*"', block))
        assert count > 0
        corpus_case_counts[name] = count
        corpus_parts.append(block)
    corpus = header + '// Official Unicode 17 GraphemeBreakTest cases retained by the pinned crate.\n'
    corpus += '\n'.join(corpus_parts) + '\n'
    outputs = {
        PREFIX + 'boundary_pages_data.rs': data.encode(),
        PREFIX + 'boundary_pages_reference.rs': reference.encode(),
        PREFIX + 'boundary_pages_corpus.rs': corpus.encode(),
        'THIRD_PARTY_LICENSES/unicode-segmentation/LICENSE-MIT': inputs['LICENSE-MIT'],
        'THIRD_PARTY_LICENSES/unicode-segmentation/COPYRIGHT': inputs['COPYRIGHT'],
    }
    provenance = {
        'crate': {'name': 'unicode-segmentation', 'version': VERSION, 'sha256': PACKAGE_SHA,
                  'locked_checksum_verified': True},
        'unicode_version': list(UNICODE_VERSION),
        'source_files': {name: {'sha256': sha(raw), 'bytes': len(raw)} for name, raw in inputs.items()},
        'generator_sha256': sha(Path(__file__).read_bytes()),
        'source_range_count': len(ranges),
        'page_size_codepoints': PAGE_SIZE,
        'production_table_bytes': len(words) * 8,
        'pages_by_class': {str(value): pages.count(value) for value in (0, 1, 2)},
        'exhaustive_audit': coverage,
        'corpus_case_counts': corpus_case_counts,
        'oracle': 'Independent bisect lookup of all original ranges, not the dense classification used by generation',
        'surrogates': 'Excluded from scalar audit; char cannot contain them. Conservative page generation includes their original category.',
        'coverage_interpretation': 'Domain counts include unassigned code points; they are not workload or language coverage percentages.',
        'license_option': 'MIT; original attribution and complete license retained',
        'generator_path': 'scripts/generate-grapheme-tables.py',
        'outputs': {name: {'sha256': sha(raw), 'bytes': len(raw)} for name, raw in outputs.items()},
    }
    outputs['src/grid/boundary_pages_manifest.json'] = (json.dumps(provenance, indent=2) + '\n').encode()
    if args.check:
        mismatches = [name for name, data in outputs.items()
                      if not (ROOT / name).is_file() or (ROOT / name).read_bytes() != data]
        assert not mismatches, 'generated outputs differ: ' + ', '.join(mismatches)
    else:
        for name, data in outputs.items():
            destination = ROOT / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            if not destination.is_file() or destination.read_bytes() != data:
                destination.write_bytes(data)
    print(json.dumps({'check_only': args.check, 'passed': True, 'coverage': coverage,
                      'corpus_case_counts': corpus_case_counts, 'production_table_bytes': len(words) * 8}, indent=2))

if __name__ == '__main__':
    main()
