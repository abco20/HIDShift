import csv
import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]


class PartitionLayoutTests(unittest.TestCase):
    def test_dual_ota_slots_and_data_partitions_fit_four_megabytes(self) -> None:
        rows = {}
        with (ROOT / "partitions" / "bridge.csv").open(encoding="utf-8") as source:
            for row in csv.reader(line for line in source if not line.startswith("#")):
                name, kind, subtype, offset, size, *_ = [value.strip() for value in row]
                rows[name] = {
                    "kind": kind,
                    "subtype": subtype,
                    "offset": int(offset, 0),
                    "size": int(size, 0),
                }

        self.assertEqual(rows["ota_0"]["size"], 0x180000)
        self.assertEqual(rows["ota_1"]["size"], 0x180000)
        self.assertEqual(rows["otadata"]["subtype"], "ota")
        self.assertEqual(rows["bridge"]["offset"], 0x320000)
        self.assertEqual(rows["mirror"]["offset"], 0x324000)

        ordered = sorted(rows.items(), key=lambda item: item[1]["offset"])
        for (_, left), (_, right) in zip(ordered, ordered[1:]):
            self.assertLessEqual(left["offset"] + left["size"], right["offset"])
        self.assertLessEqual(
            max(value["offset"] + value["size"] for value in rows.values()),
            4 * 1024 * 1024,
        )


if __name__ == "__main__":
    unittest.main()
