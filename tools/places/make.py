"""Makes crates/seeing/data/places.tsv, the places photos are named after, from GeoNames (CC BY 4.0,
https://www.geonames.org): every place of 15,000 people or more, with its region and country, for naming where a photo
was taken from its GPS position, on this PC.

python -I make.py <folder with cities15000.txt, admin1CodesASCII.txt, countryInfo.txt> <out.tsv>
"""

import sys
from pathlib import Path


def main():
    source, out = Path(sys.argv[1]), Path(sys.argv[2])
    countries = {}
    for line in (source / "countryInfo.txt").read_text(encoding="utf-8").splitlines():
        if line and not line.startswith("#"):
            fields = line.split("\t")
            countries[fields[0]] = fields[4]
    regions = {}
    for line in (source / "admin1CodesASCII.txt").read_text(encoding="utf-8").splitlines():
        fields = line.split("\t")
        regions[fields[0]] = fields[1]
    rows = []
    for line in (source / "cities15000.txt").read_text(encoding="utf-8").splitlines():
        fields = line.split("\t")
        name, lat, lon, country, admin1, people = fields[1], float(fields[4]), float(fields[5]), fields[8], fields[10], int(fields[14] or 0)
        region = regions.get(f"{country}.{admin1}", "")
        rows.append(f"{name}\t{region}\t{countries.get(country, country)}\t{people}\t{lat:.4f}\t{lon:.4f}")
    rows.sort()
    header = "# GeoNames (https://www.geonames.org), CC BY 4.0: places of 15,000 people or more. name, region, country, people, latitude, longitude.\n"
    out.write_text(header + "\n".join(rows) + "\n", encoding="utf-8", newline="\n")
    print(len(rows), "places,", out.stat().st_size, "bytes")


if __name__ == "__main__":
    main()
