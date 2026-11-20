# Machine configuration

## Storage and XML

Libvirt holds the configuration for registered machines. Use **Hardware →
Advanced → Edit XML** to inspect or change it. Creation, appliance import, and
**Move storage…** define the machine in libvirt without writing a configuration
copy beside the disks. A hidden ownership record lets Hydroxl identify the
disk files it created in custom storage folders. Older folders can still use
their original XML for disk ownership checks.

**Export folder…** writes one `machine.xml` with the paths to the exported
disk copies. Import it from the library's **Import libvirt XML…** action or
open it with Hydroxl. If you relocate the export, update the file paths in the
XML before importing. Libvirt diagnostics appear in the import form if
registration fails.

## Ask for hardware preferences on import

Folder exports automatically include this namespaced tag under `<metadata>`.
Add it to an existing libvirt domain XML to opt into the same import behavior.

```xml
<metadata>
  <hydroxl:review-hardware xmlns:hydroxl="https://hydroxl.dev/xml/import/1"/>
</metadata>
```

If the XML already has a `<metadata>` element, place the tag inside it. The
import form reads the virtual CPU count and memory size from the XML and lets
you change both before defining the guest. Leaving the CPU count as supplied
preserves its topology. Opening XML without the tag shows the standard import
fields.

## Change a drive target

Power off the machine and open **Hardware → Storage → drive actions → Edit
drive…**. The form starts with the current bus and target. A bus change
suggests the corresponding target name, which you can edit before saving.
VirtIO uses targets such as `vda` and `vdc`. SATA, SCSI, and USB use `sd`
targets such as `sdb`, and IDE uses `hd` targets such as `hda`.

The target changes in the saved libvirt definition. The guest OS assigns its
own runtime device names and may need different storage drivers after a bus
change. The drive still points to its existing image and retains its
configured boot priority.
