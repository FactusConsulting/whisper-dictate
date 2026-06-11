"""Ad-hoc probe: which (rate, channels, dtype, auto_convert) opens the Yeti?

Run with the worker's venv python (the one that has sounddevice):

    C:\\Users\\larsm\\voice-pi-venv\\Scripts\\python.exe probe_yeti.py

It prints the Yeti's reported capabilities, then tries to OPEN + START an
InputStream for every combination and reports which ones actually work. Paste
the whole output back. Nothing is recorded; streams are opened ~50ms and closed.
"""
import sys
import sounddevice as sd

TARGET = "Yeti"  # substring match on device name


def find_devices():
    hits = []
    for idx, d in enumerate(sd.query_devices()):
        if d.get("max_input_channels", 0) > 0 and TARGET.lower() in d["name"].lower():
            hits.append((idx, d))
    return hits


def main():
    print("=== host APIs ===")
    for i, ha in enumerate(sd.query_hostapis()):
        print(f"  [{i}] {ha['name']}  default_input={ha.get('default_input_device')}")

    hits = find_devices()
    if not hits:
        print(f"!! no input device matching {TARGET!r} found")
        print("All input devices:")
        for idx, d in enumerate(sd.query_devices()):
            if d.get("max_input_channels", 0) > 0:
                print(f"  [{idx}] {d['name']} (hostapi {d['hostapi']})")
        return

    for idx, d in hits:
        ha = sd.query_hostapis(d["hostapi"])
        print(f"\n=== device [{idx}] {d['name']} ===")
        print(f"  hostapi      : [{d['hostapi']}] {ha['name']}")
        print(f"  max_in_chans : {d['max_input_channels']}")
        print(f"  default_sr   : {d.get('default_samplerate')}")
        print(f"  low/high lat : {d.get('default_low_input_latency')} / {d.get('default_high_input_latency')}")

        has_wasapi = hasattr(sd, "WasapiSettings")
        rates = [16000, 22050, 44100, 48000]
        dsr = d.get("default_samplerate")
        if dsr and int(round(dsr)) not in rates:
            rates.append(int(round(dsr)))
        chans = sorted({1, 2, d["max_input_channels"]})
        dtypes = ["int16", "float32"]
        autoconv = [False, True] if has_wasapi else [False]

        print(f"  WasapiSettings available: {has_wasapi}")
        print("\n  --- open+start probe (only WORKING combos and first error per group) ---")
        for ac in autoconv:
            for rate in rates:
                for ch in chans:
                    for dt in dtypes:
                        kw = dict(device=idx, samplerate=rate, channels=ch,
                                  dtype=dt, blocksize=0)
                        if ac:
                            kw["extra_settings"] = sd.WasapiSettings(auto_convert=True)
                        try:
                            st = sd.InputStream(**kw)
                            st.start()
                            st.stop()
                            st.close()
                            print(f"  OK   auto_convert={ac} rate={rate} ch={ch} dtype={dt}")
                        except Exception as exc:
                            msg = str(exc).splitlines()[0][:90]
                            print(f"  fail auto_convert={ac} rate={rate} ch={ch} dtype={dt}  -> {msg}")
    print("\n=== done ===")


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # noqa: BLE001
        print("PROBE CRASHED:", exc, file=sys.stderr)
        raise
