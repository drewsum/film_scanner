# scan-compare

Measures scans of the same frame side by side, to compare this app's exports with another
scanner program's (issue #28): sharpness along each axis, grain, how well the color channels
line up, black and white points, contrast, saturation, and the TIFFs' profile and resolution
tags. What each row means is at the top of `compare.py`.

```sh
pip install numpy scipy tifffile pillow
python compare.py roll187_012.tif test_06.tif
```

The first file is the reference for the `finest` row.
