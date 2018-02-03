// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use rustc::util::common;
use rustc::middle::cstore::MetadataLoader;
use rustc_back::target::Target;
use llvm;
use llvm::{False, ObjectFile, mk_section_iter};
use llvm::archive_ro::ArchiveRO;

use rustc_data_structures::owning_ref::{ErasedBoxRef, OwningRef};
use std::path::Path;
use std::ptr;
use std::slice;

pub const METADATA_FILENAME: &str = "rust.metadata.bin";

pub struct LlvmMetadataLoader;

impl MetadataLoader for LlvmMetadataLoader {
    fn get_rlib_metadata(&self, _: &Target, filename: &Path) -> Result<ErasedBoxRef<[u8]>, String> {
        // Use ArchiveRO for speed here, it's backed by LLVM and uses mmap
        // internally to read the file. We also avoid even using a memcpy by
        // just keeping the archive along while the metadata is in use.
        let archive = ArchiveRO::open(filename)
            .map(|ar| OwningRef::new(box ar))
            .map_err(|e| {
                debug!("llvm didn't like `{}`: {}", filename.display(), e);
                format!("failed to read rlib metadata in '{}': {}", filename.display(), e)
            })?;
        let buf: OwningRef<_, [u8]> = archive
            .try_map(|ar| {
                ar.iter()
                    .filter_map(|s| s.ok())
                    .find(|sect| sect.name() == Some(METADATA_FILENAME))
                    .map(|s| s.data())
                    .ok_or_else(|| {
                        debug!("didn't find '{}' in the archive", METADATA_FILENAME);
                        format!("failed to read rlib metadata: '{}'",
                                filename.display())
                    })
            })?;
        Ok(buf.erase_owner())
    }

    fn get_dylib_metadata(&self,
                          target: &Target,
                          filename: &Path)
                          -> Result<ErasedBoxRef<[u8]>, String> {
        unsafe {
            let buf = common::path2cstr(filename);
            let mb = llvm::LLVMRustCreateMemoryBufferWithContentsOfFile(buf.as_ptr());
            if mb as isize == 0 {
                return Err(format!("error reading library: '{}'", filename.display()));
            }
            let of = ObjectFile::new(mb)
                .map(|of| OwningRef::new(box of))
                .ok_or_else(|| format!("provided path not an object file: '{}'",
                                        filename.display()))?;
            let buf = of.try_map(|of| search_meta_section(of, target, filename))?;
            Ok(buf.erase_owner())
        }
    }
}

fn search_meta_section<'a>(of: &'a ObjectFile,
                           target: &Target,
                           filename: &Path)
                           -> Result<&'a [u8], String> {
    unsafe {
        let si = mk_section_iter(of.llof);
        while llvm::LLVMIsSectionIteratorAtEnd(of.llof, si.llsi) == False {
            let mut name_buf = ptr::null();
            let name_len = llvm::LLVMRustGetSectionName(si.llsi, &mut name_buf);
            let name = slice::from_raw_parts(name_buf as *const u8, name_len as usize).to_vec();
            let name = String::from_utf8(name).unwrap();
            debug!("get_metadata_section: name {}", name);
            if read_metadata_section_name(target) == name {
                let cbuf = llvm::LLVMGetSectionContents(si.llsi);
                let csz = llvm::LLVMGetSectionSize(si.llsi) as usize;
                // The buffer is valid while the object file is around
                let buf: &'a [u8] = slice::from_raw_parts(cbuf as *const u8, csz);
                return Ok(buf);
            }
            llvm::LLVMMoveToNextSection(si.llsi);
        }
    }
    Err(format!("metadata not found: '{}'", filename.display()))
}

pub fn metadata_section_name(target: &Target) -> &'static str {
    // Historical note:
    //
    // When using link.exe it was seen that the section name `.note.rustc`
    // was getting shortened to `.note.ru`, and according to the PE and COFF
    // specification:
    //
    // > Executable images do not use a string table and do not support
    // > section names longer than 8 characters
    //
    // https://msdn.microsoft.com/en-us/library/windows/hardware/gg463119.aspx
    //
    // As a result, we choose a slightly shorter name! As to why
    // `.note.rustc` works on MinGW, that's another good question...

    if target.options.is_like_osx {
        "__DATA,.rustc"
    } else {
        ".rustc"
    }
}

fn read_metadata_section_name(_target: &Target) -> &'static str {
    ".rustc"
}
