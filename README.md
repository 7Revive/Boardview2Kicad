So i made this Boardview2Kicad extractor/converter, thusfar it can only do .FZ boardview files to kicad files, it extracts footprints, netlists, full pinouts, and also the entire pcb, but it doesnt include copper tracks as boardviews dont contain them.
Build using "cargo build --release".
To run it use "fz2kicad convert <path/to/boardview/boardview.fz> <output_directory>"
