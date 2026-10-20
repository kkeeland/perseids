# ruby test/split_test.rb — exec --split's logic, on your machine (no box).
require "minitest/autorun"
require "tmpdir"
require_relative "../lib/split"

class SplitTest < Minitest::Test
  def test_glob_matches_the_commit_not_the_disk
    Dir.mktmpdir do |d|
      Dir.chdir(d) do
        system("git init -q && git config user.email t@t && git config user.name t") || flunk
        { "test/a_test.rb" => 1, "test/deep/b_test.rb" => 30, "test/with space_test.rb" => 5, "lib/c.rb" => 9 }.each do |f, n|
          FileUtils.mkdir_p(File.dirname(f))
          File.write(f, "x" * n)
        end
        system("git add -A && git commit -qm t") || flunk
        File.write("test/untracked_test.rb", "")
        system("git rm -q test/a_test.rb") || flunk # staged, not committed: HEAD still has it
        sha = `git rev-parse HEAD`.strip
        assert_equal({ "test/a_test.rb" => 1, "test/deep/b_test.rb" => 30, "test/with space_test.rb" => 5 },
                     PerseidsSplit.files(sha, "test/**/*_test.rb"))
        assert_equal({}, PerseidsSplit.files(sha, "spec/**/*.rb"))
      end
    end
  end

  def test_groups_balance_by_size_deterministically
    f = { "a" => 10, "b" => 9, "c" => 5, "d" => 5, "e" => 1, "f" => 0 }
    assert_equal [%w[a f], %w[b e], %w[c d]], PerseidsSplit.groups(f, 3)
    assert_equal PerseidsSplit.groups(f, 3), PerseidsSplit.groups(f.to_a.reverse.to_h, 3)
    assert_equal [%w[a b c d e f]], PerseidsSplit.groups(f, 1)
  end

  def test_placeholders_and_quoting
    assert_equal "jest --shard=2/8", PerseidsSplit.command("jest --shard={i}/{n}", 2, 8)
    assert_equal "pytest --splits 4 --group 1", PerseidsSplit.command("pytest --splits {n} --group {i}", 1, 4)
    assert_equal "npm test", PerseidsSplit.command("npm test", 3, 4)
    files = ["test/with space_test.rb", "test/it's_test.rb"]
    c = PerseidsSplit.command("ruby -e 'p ARGV' {files} # {i}", 1, 2, files)
    assert_equal "ruby -e 'p ARGV' test/with\\ space_test.rb test/it\\'s_test.rb # 1", c
    assert_equal files.inspect, `bash -c #{Shellwords.escape(c)}`.strip # the box's shell gets the names back intact
    assert_equal "rspec a b", PerseidsSplit.command("rspec", 1, 1, %w[a b]) # no {files}: appended
  end
end
