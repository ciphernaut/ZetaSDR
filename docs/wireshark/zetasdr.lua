-- ZetaSDR's Mode S packets for Wireshark.
--
-- A ZetaSDR node writes each Mode S frame an ADS-B job decodes as one packet under the
-- private link type DLT_USER1 (148), the frame's 7 or 14 bytes as received. This
-- dissector reads them: downlink format, capability, address, the extended squitter's
-- ME field and type code, and the parity. It is written from the Mode S frame format
-- (ICAO Annex 10, Volume IV) and checks no parity.
--
-- An APRS job served by OnAir writes its frames as bare AX.25 (link type 3,
-- LINKTYPE_AX25): Wireshark's own AX.25 dissector reads them, so this file has nothing
-- to add for them.
--
-- Install: copy this file to Wireshark's personal Lua plugins directory
-- (`~/.local/lib/wireshark/plugins/` on Linux; Help > About Wireshark > Folders names
-- it), or load it for one run: `wireshark -X lua_script:zetasdr.lua capture.pcapng`.
--
-- Written for the Lua that Wireshark embeds (5.2 to 5.4): bits are read with
-- TvbRange:bitfield, so no 5.3 operators are needed.

local modes = Proto("zetasdr_modes", "ZetaSDR Mode S")

local f = modes.fields
f.df = ProtoField.uint8("zetasdr_modes.df", "Downlink format", base.DEC)
f.ca = ProtoField.uint8("zetasdr_modes.ca", "Capability (CA/CF)", base.DEC)
f.aa = ProtoField.uint24("zetasdr_modes.aa", "Address", base.HEX)
f.me = ProtoField.bytes("zetasdr_modes.me", "Message, extended squitter (ME)")
f.tc = ProtoField.uint8("zetasdr_modes.tc", "Type code", base.DEC)
f.parity = ProtoField.uint24("zetasdr_modes.parity", "Parity", base.HEX)

-- The downlink formats whose bytes 1 to 3 are the address in the clear (all-call reply,
-- extended squitter, non-transponder extended squitter); the others overlay it on the
-- parity.
local ADDRESSED = { [11] = true, [17] = true, [18] = true }
-- The downlink formats that carry an ME field and its type code.
local SQUITTER = { [17] = true, [18] = true }

function modes.dissector(tvb, pinfo, tree)
    local len = tvb:len()
    if len ~= 7 and len ~= 14 then
        return 0
    end
    pinfo.cols.protocol = "Mode S"
    local subtree = tree:add(modes, tvb(), "ZetaSDR Mode S")

    -- The first five bits; formats 24 and up are told by their first two bits alone.
    local df = tvb(0, 1):bitfield(0, 5)
    if df >= 24 then
        df = 24
    end
    subtree:add(f.df, tvb(0, 1), df)

    local info = "DF" .. df .. " (address in parity)"
    if ADDRESSED[df] then
        subtree:add(f.ca, tvb(0, 1), tvb(0, 1):bitfield(5, 3))
        subtree:add(f.aa, tvb(1, 3))
        info = string.format("DF%d %06X", df, tvb(1, 3):uint())
    end
    if SQUITTER[df] and len == 14 then
        subtree:add(f.me, tvb(4, 7))
        subtree:add(f.tc, tvb(4, 1), tvb(4, 1):bitfield(0, 5))
    end
    subtree:add(f.parity, tvb(len - 3, 3))
    pinfo.cols.info = info
    return len
end

local encaps = wtap_encaps or wtap
DissectorTable.get("wtap_encap"):add(encaps.USER1, modes)
