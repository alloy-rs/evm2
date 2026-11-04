#----------------------------------------------------------------
# Generated CMake target import file for configuration "Release".
#----------------------------------------------------------------

# Commands may need to know the format version.
set(CMAKE_IMPORT_FILE_VERSION 1)

# Import target "mcl::mcl" for configuration "Release"
set_property(TARGET mcl::mcl APPEND PROPERTY IMPORTED_CONFIGURATIONS RELEASE)
set_target_properties(mcl::mcl PROPERTIES
  IMPORTED_LOCATION_RELEASE "${_IMPORT_PREFIX}/lib/libmcl.so.3.3"
  IMPORTED_SONAME_RELEASE "libmcl.so.3"
  )

list(APPEND _cmake_import_check_targets mcl::mcl )
list(APPEND _cmake_import_check_files_for_mcl::mcl "${_IMPORT_PREFIX}/lib/libmcl.so.3.3" )

# Import target "mcl::mcl_st" for configuration "Release"
set_property(TARGET mcl::mcl_st APPEND PROPERTY IMPORTED_CONFIGURATIONS RELEASE)
set_target_properties(mcl::mcl_st PROPERTIES
  IMPORTED_LINK_INTERFACE_LANGUAGES_RELEASE "ASM;CXX"
  IMPORTED_LOCATION_RELEASE "${_IMPORT_PREFIX}/lib/libmcl.a"
  )

list(APPEND _cmake_import_check_targets mcl::mcl_st )
list(APPEND _cmake_import_check_files_for_mcl::mcl_st "${_IMPORT_PREFIX}/lib/libmcl.a" )

# Commands beyond this point should not need to know the version.
set(CMAKE_IMPORT_FILE_VERSION)
