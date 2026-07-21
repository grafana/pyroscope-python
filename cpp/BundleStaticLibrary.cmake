# bundle_static_library(<tgt> <bundled_name>)
#
# Recursively collects every STATIC_LIBRARY in the link closure of <tgt> (our library plus
# the absl libraries pulled in transitively by absl::flat_hash_map) and merges them into a
# single archive lib<bundled_name>.a. The Rust cdylib link step (rust/build.rs) is not
# CMake-aware and cannot follow CMake's transitive link deps, so it links this one merged
# archive instead.
function(bundle_static_library tgt_name bundled_tgt_name)
    list(APPEND static_libs ${tgt_name})

    function(_recursively_collect_dependencies input_target)
        set(_input_link_libraries LINK_LIBRARIES)
        get_target_property(_input_type ${input_target} TYPE)
        if (${_input_type} STREQUAL "INTERFACE_LIBRARY")
            set(_input_link_libraries INTERFACE_LINK_LIBRARIES)
        endif ()
        get_target_property(public_dependencies ${input_target} ${_input_link_libraries})
        foreach (dependency IN LISTS public_dependencies)
            if (TARGET ${dependency})
                get_target_property(alias ${dependency} ALIASED_TARGET)
                if (TARGET ${alias})
                    set(dependency ${alias})
                endif ()
                get_target_property(_type ${dependency} TYPE)
                if (${_type} STREQUAL "STATIC_LIBRARY")
                    list(APPEND static_libs ${dependency})
                endif ()
                get_property(library_already_added GLOBAL PROPERTY _${tgt_name}_static_bundle_${dependency})
                if (NOT library_already_added)
                    set_property(GLOBAL PROPERTY _${tgt_name}_static_bundle_${dependency} ON)
                    _recursively_collect_dependencies(${dependency})
                endif ()
            endif ()
        endforeach ()
        set(static_libs ${static_libs} PARENT_SCOPE)
    endfunction()

    _recursively_collect_dependencies(${tgt_name})
    list(REMOVE_DUPLICATES static_libs)

    set(bundled_tgt_full_name
            ${CMAKE_BINARY_DIR}/${CMAKE_STATIC_LIBRARY_PREFIX}${bundled_tgt_name}${CMAKE_STATIC_LIBRARY_SUFFIX})

    # Per-lib archive paths, resolved at generate time.
    set(static_lib_files "")
    foreach (lib IN LISTS static_libs)
        list(APPEND static_lib_files $<TARGET_FILE:${lib}>)
    endforeach ()

    if (APPLE)
        find_program(libtool_path libtool REQUIRED)
        add_custom_command(
                COMMAND ${libtool_path} -static -o ${bundled_tgt_full_name} ${static_lib_files}
                OUTPUT ${bundled_tgt_full_name}
                DEPENDS ${static_libs}
                COMMENT "Bundling ${bundled_tgt_name}"
                VERBATIM)
    else ()
        # GNU ar MRI script: create the archive, addlib each input, save.
        set(ar_script_content "CREATE ${bundled_tgt_full_name}\n")
        foreach (lib IN LISTS static_libs)
            string(APPEND ar_script_content "ADDLIB $<TARGET_FILE:${lib}>\n")
        endforeach ()
        string(APPEND ar_script_content "SAVE\nEND\n")
        set(ar_script ${CMAKE_BINARY_DIR}/${bundled_tgt_name}.ar)
        file(GENERATE OUTPUT ${ar_script} CONTENT ${ar_script_content})
        add_custom_command(
                COMMAND ${CMAKE_AR} -M < ${ar_script}
                OUTPUT ${bundled_tgt_full_name}
                DEPENDS ${static_libs}
                COMMENT "Bundling ${bundled_tgt_name}"
                VERBATIM)
    endif ()

    add_custom_target(_bundle_${bundled_tgt_name} ALL DEPENDS ${bundled_tgt_full_name})
    add_dependencies(_bundle_${bundled_tgt_name} ${tgt_name})

    install(FILES ${bundled_tgt_full_name} DESTINATION .)
endfunction()
