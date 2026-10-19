# ruby test/bisect_test.rb — perseids bisect's logic, on your machine (a throwaway git repo, no box).
require "minitest/autorun"
require "tmpdir"
require_relative "../lib/bisect"

class BisectTest < Minitest::Test
  B = Perseids::Bisect

  def test_sample_keeps_both_ends
    assert_equal (0..9).to_a, B.sample((0..9).to_a, 30)
    assert_equal [0, 25, 50, 74, 99], B.sample((0..99).to_a, 5)
    assert_equal [0, 2, 3], B.sample((0..3).to_a, 3) # always an inner commit, so narrowing gets smaller
  end

  def test_verdict
    v = B.verdict(%w[pass pass fail fail])
    assert_equal [1, 2], [v[:good], v[:bad]]
    refute v[:flip] || v[:good_fails] || v[:bad_passes]
    v = B.verdict(["pass", nil, nil, "fail"]) # sampled: the break is somewhere in the gap
    assert_equal [0, 3], [v[:good], v[:bad]]
    assert_equal [1, 2], B.verdict(%w[pass fail pass fail])[:flip]
    assert B.verdict(%w[fail fail])[:good_fails]
    assert B.verdict(%w[pass pass])[:bad_passes]
    assert_equal 1, B.verdict(%w[pass error fail])[:errors]
    assert_nil B.verdict(%w[pass error fail])[:bad]
  end

  def test_range
    Dir.mktmpdir do |d|
      Dir.chdir(d) do
        ENV.update("GIT_AUTHOR_NAME" => "t", "GIT_AUTHOR_EMAIL" => "t@t", "GIT_COMMITTER_NAME" => "t", "GIT_COMMITTER_EMAIL" => "t@t")
        system("git init -q . && for i in $(seq 12); do git commit -q --allow-empty -m c$i; done", exception: true)
        all, tested = B.range("", "HEAD", 30) # no origin: the root, since the history is shorter than n
        assert_equal (1..12).map { |i| "c#{i}" }, all.map(&:last)
        assert_equal all, tested
        all, tested = B.range("HEAD~11", "HEAD", 5)
        assert_equal 12, all.size
        assert_equal %w[c1 c4 c7 c9 c12], tested.map(&:last)
        assert_equal %w[c9 c10 c11 c12], B.range("", "HEAD", 4)[0].map(&:last) # n commits back
        system("git update-ref refs/remotes/origin/main HEAD~3", exception: true)
        assert_equal "c9", B.range("", "HEAD", 30)[0].first.last # the merge-base with origin
        assert_raises(RuntimeError) { B.range("HEAD", "HEAD~2", 30) }
        assert_raises(RuntimeError) { B.range("", "HEAD", 2) }
      end
    end
  end
end
