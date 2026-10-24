#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Decode a usbmon pcap (LINKTYPE_USB_LINUX_MMAPPED, 220) for one device and line it up with the action log.

Shows host->device data (control SET_REPORT / interrupt OUT submissions) and
device->host replies on non-mouse endpoints (GET_REPORT / vendor interrupt IN completions).

usage: decode.py capture.pcap actions.log [devnum] [--all]
"""
import struct, sys, datetime

def read_pcap(path):
    with open(path, 'rb') as f:
        gh = f.read(24)
        magic = struct.unpack('<I', gh[:4])[0]
        assert magic in (0xa1b2c3d4, 0xa1b23c4d), hex(magic)
        nano = magic == 0xa1b23c4d
        linktype = struct.unpack('<I', gh[20:24])[0]
        assert linktype in (189, 220), linktype
        while True:
            ph = f.read(16)
            if len(ph) < 16:
                return
            ts_s, ts_f, incl, orig = struct.unpack('<IIII', ph)
            data = f.read(incl)
            yield ts_s + ts_f / (1e9 if nano else 1e6), data, linktype

def parse(pkt, linktype):
    hdr_len = 64 if linktype == 220 else 48
    (urb_id, typ, xfer, ep, dev, bus, fsetup, fdata, tsec, tusec, status,
     length, len_cap) = struct.unpack('<QcBBBHccqiiII', pkt[:40])
    setup = pkt[40:48]
    payload = pkt[hdr_len:hdr_len + len_cap]
    return dict(type=typ.decode(), xfer=xfer, ep=ep, dev=dev, setup=setup,
                fsetup=fsetup, status=status, length=length, data=payload)

def load_actions(path):
    acts = []
    for line in open(path):
        t, _, label = line.strip().partition(' ')
        acts.append((t, label))
    return acts

def main():
    pcap, actlog = sys.argv[1], sys.argv[2]
    devnum = int(sys.argv[3]) if len(sys.argv) > 3 and sys.argv[3].isdigit() else 13
    show_all = '--all' in sys.argv
    acts = load_actions(actlog)
    ai = 0
    XF = {0: 'ISO', 1: 'INT', 2: 'CTL', 3: 'BLK'}
    for ts, pkt, lt in read_pcap(pcap):
        p = parse(pkt, lt)
        if p['dev'] != devnum:
            continue
        tstr = datetime.datetime.fromtimestamp(ts).strftime('%H:%M:%S.%f')
        while ai < len(acts) and acts[ai][0] <= tstr:
            print(f"\n######## {acts[ai][0]}  ACTION: {acts[ai][1]}")
            ai += 1
        epn = p['ep'] & 0x7f
        is_in = bool(p['ep'] & 0x80)
        d = p['data']
        line = None
        if p['xfer'] == 2 and p['type'] == 'S' and p['fsetup'] == b'\x00':
            bmrt, breq, wval, widx, wlen = struct.unpack('<BBHHH', p['setup'])
            if bmrt == 0x21 and breq == 0x09:      # SET_REPORT
                line = f"SET_REPORT  if={widx} rid=0x{wval & 0xff:02x} type={wval >> 8} len={len(d)}: {d.hex(' ')}"
            elif bmrt == 0xa1 and breq == 0x01:    # GET_REPORT request
                line = f"GET_REPORT? if={widx} rid=0x{wval & 0xff:02x} len={wlen}"
            elif show_all:
                line = f"CTL setup {p['setup'].hex(' ')} data={d.hex(' ')}"
        elif p['xfer'] == 2 and p['type'] == 'C' and is_in and d:
            line = f"  CTL reply len={len(d)}: {d.hex(' ')}"
        elif p['xfer'] == 1 and not is_in and p['type'] == 'S' and d:
            line = f"INT OUT ep{epn} len={len(d)}: {d.hex(' ')}"
        elif p['xfer'] == 1 and is_in and p['type'] == 'C' and d and (epn != 1 or show_all):
            line = f"  INT IN ep{epn} len={len(d)}: {d.hex(' ')}"
        if line:
            print(f"{tstr} {line}")

if __name__ == '__main__':
    main()
