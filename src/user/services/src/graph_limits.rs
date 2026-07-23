pub const MAX_MODULES: usize = bootproto::elf::MAX_DYNAMIC_MODULES;

pub fn can_visit(depth: usize, loaded_modules: usize, already_visiting: bool) -> bool {
	depth < bootproto::elf::MAX_DYNAMIC_DEPENDENCY_DEPTH && loaded_modules < MAX_MODULES && !already_visiting
}

#[cfg(test)]
#[path = "graph_limits/tests.rs"]
mod tests;
