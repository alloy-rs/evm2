macro_rules! normal_tables {
    () => {
        /// Normal instruction dispatch table.
        pub(super) type RawInstrTable<T> = [RawInstrFn<T>; 256];

        pub(crate) const fn make_table<T, C, M>(
            previous: Option<&RawInstrTable<T>>,
            previous_version_tables: Option<&crate::VersionTables<T>>,
        ) -> RawInstrTable<T>
        where
            T: crate::EvmTypes,
            C: crate::EvmConfig<T>,
            M: super::InspectMode<T>,
        {
            let mut table = match previous {
                Some(previous) => *previous,
                None => [dispatch::<T, C, M, 0, true> as super::InstrFn<T>; 256],
            };
            let vt = C::VERSION_TABLES;
            for_each_opcode_value!([table, T, C, M, vt, previous_version_tables, dispatch, super::InstrFn<T>] assign_normal_instruction_table_entries);

            // Make all unknown entries point to the same dispatch function.
            let mut i = 0;
            let mut unknown_idx = None;
            while i < 256 {
                if C::VERSION_TABLES.is_unknown_opcode(i as u8) {
                    if unknown_idx.is_none() {
                        unknown_idx = Some(i);
                    }
                    table[i] = table[unknown_idx.unwrap()];
                }
                i += 1;
            }

            table
        }

        pub(crate) const fn make_selector_tables<
            T,
            F,
            M,
            const CUSTOM_SPEC_ID: u8,
        >() -> [RawInstrTable<T>; crate::SpecId::COUNT]
        where
            T: crate::EvmTypes,
            F: crate::EvmConfigSelector<T>,
            M: super::InspectMode<T>,
        {
            crate::for_each_spec!([] make_normal_selector_tables)
        }
    };
}

pub(crate) use normal_tables;
